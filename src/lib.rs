mod bitstream;
mod huffman;
pub mod utils;
use bitstream::BitReader;
use huffman::Huffman;
use utils::*;

use std::convert::TryFrom;
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};

// Define constraints for underlaying Chd file I/O
pub trait R: Read + Seek {}
impl<T: Read + Seek> R for T {}

const V5: u32 = 5;

/* codec #0
 * these types are live when running */
const COMPRESSION_TYPE_0: u8 = 0;
/* codec #1 */
const COMPRESSION_TYPE_1: u8 = 1;
/* codec #2 */
const COMPRESSION_TYPE_2: u8 = 2;
/* codec #3 */
const COMPRESSION_TYPE_3: u8 = 3;
/* no compression; implicit length = hunkbytes */
const COMPRESSION_NONE: u8 = 4;
/* same as another block in this chd */
const COMPRESSION_SELF: u8 = 5;
/* same as a hunk's worth of units in the parent chd */
const COMPRESSION_PARENT: u8 = 6;

/* start of small RLE run (4-bit length)
 * these additional pseudo-types are used for compressed encodings: */
const COMPRESSION_RLE_SMALL: u8 = 7;
/* start of large RLE run (8-bit length) */
const COMPRESSION_RLE_LARGE: u8 = 8;
/* same as the last COMPRESSION_SELF block */
const COMPRESSION_SELF_0: u8 = 9;
/* same as the last COMPRESSION_SELF block + 1 */
const COMPRESSION_SELF_1: u8 = 10;
/* same block in the parent */
const COMPRESSION_PARENT_SELF: u8 = 11;
/* same as the last COMPRESSION_PARENT block */
const COMPRESSION_PARENT_0: u8 = 12;
/* same as the last COMPRESSION_PARENT block + 1 */
const COMPRESSION_PARENT_1: u8 = 13;

// Different drive versions have different map format
trait Map {
    // Return hunk offset in file
    fn locate(&self, hunknum: usize) -> u64;
}

type MapType = Box<dyn Map>;

#[derive(Default)]
struct Header {
    // V5 fields
    length: u32,           // length of header (including tag and length fields)
    version: u32,          // drive format version
    compressors: [u32; 4], // which custom compressors are used?
    size: u64,             // logical size of the data (in bytes)
    mapoffset: u64,        // offset to the map
    metaoffset: u64,       // offset to the first blob of metadata
    hunkbytes: u32,        // number of bytes per hunk (512k maximum)
    unitbytes: u32,        // number of bytes per unit within each hunk
    rawsha1: [u8; 20],     // raw data SHA1
    sha1: [u8; 20],        // combined raw+meta SHA1
    parentsha1: [u8; 20],  // combined raw+meta SHA1 of parent
    // V4 fields
    hunkcount: u32, // total # of hunks represented
                    // flags: u32,
}

impl Header {
    fn read<T: R>(io: &mut T) -> io::Result<(Self, MapType)> {
        let mut data = [0u8; 124];
        io.read_at(0, &mut data)?;

        let magic = &data[0..8];
        if magic != b"MComprHD" {
            return Err(invalid_data(format!("chd: invalid magic {:02x?}", magic)));
        }

        let mut header = Header::default();
        header.length = read_be32(&data[8..12]);
        header.version = read_be32(&data[12..16]);
        match header.version {
            V5 => {
                header.read_header_v5(&data)?;
                let map = match header.compressors[0] {
                    0 => UncompressedMap5::read(io, &header),
                    _ => CompressedMap5::read(io, &header),
                }?;
                Ok((header, map))
            }
            x => Err(invalid_data(format!("chd: unsupported version {}", x))),
        }
    }

    fn read_header_v5(&mut self, data: &[u8]) -> io::Result<()> {
        if self.length != 124 {
            return Err(invalid_data(format!(
                "hdrv5: invalid header length {}",
                self.length
            )));
        }
        self.compressors[0] = read_be32(&data[16..20]);
        self.compressors[1] = read_be32(&data[20..24]);
        self.compressors[2] = read_be32(&data[24..28]);
        self.compressors[3] = read_be32(&data[28..32]);
        self.size = read_be64(&data[32..40]);
        self.mapoffset = read_be64(&data[40..48]);
        self.metaoffset = read_be64(&data[48..56]);
        self.hunkbytes = read_be32(&data[56..60]);
        self.unitbytes = read_be32(&data[60..64]);
        copy_from(&mut self.rawsha1, &data[64..84]);
        copy_from(&mut self.sha1, &data[84..104]);
        copy_from(&mut self.parentsha1, &data[104..124]);

        // sanity checks
        if self.hunkbytes < 1 || self.hunkbytes > 512 * 1024 {
            return Err(invalid_data(format!(
                "hdrv5: invalid size of hunk {}",
                self.hunkbytes
            )));
        }
        if self.unitbytes < 1
            || self.hunkbytes < self.unitbytes
            || self.hunkbytes % self.unitbytes > 0
        {
            return Err(invalid_data(format!(
                "hdrv5: wrong size of unit {} (hunk size {})",
                self.unitbytes, self.hunkbytes
            )));
        }
        let hunkbytes = self.hunkbytes as u64;
        let hunkcount = (self.size + hunkbytes - 1) / hunkbytes;
        self.hunkcount = u32::try_from(hunkcount).map_err(|_| {
            invalid_data(format!(
                "hdrv5: hunk count {} for size {} is too big",
                hunkcount, self.size
            ))
        })?;
        Ok(())
    }
}

struct UncompressedMap5 {
    hunkbytes: u64,
    map: Vec<u8>, // uncompressed hunk map
}

impl UncompressedMap5 {
    const fn offset(hunknum: usize) -> usize {
        /*
        V5 uncompressed map format:

        [  0] uint32_t offset;        // starting offset / hunk size
        */
        4 * hunknum
    }

    fn read<T: R>(io: &mut T, header: &Header) -> io::Result<MapType> {
        let hunkcount = header.hunkcount as usize;
        let mut map = vec![0; Self::offset(hunkcount)];
        io.read_at(header.mapoffset, &mut map)?;
        Ok(Box::new(Self {
            hunkbytes: header.hunkbytes as u64,
            map,
        }))
    }
}

impl Map for UncompressedMap5 {
    fn locate(&self, hunknum: usize) -> u64 {
        let offs = Self::offset(hunknum);
        let offset = read_be32(&self.map[offs..offs + 4]) as u64;
        offset * self.hunkbytes
    }
}

struct CompressedMap5 {
    map: Vec<u8>, // uncompressed hunk map
}

impl CompressedMap5 {
    const fn offset(hunknum: usize) -> usize {
        /*
        Each compressed map entry, once expanded, looks like:

        [  0] uint8_t compression;    // compression type
        [  1] UINT24 complength;      // compressed length
        [  4] UINT48 offset;          // offset
        [ 10] uint16_t crc;           // crc-16 of the data
        */
        12 * hunknum
    }

    fn read<T: R>(io: &mut T, header: &Header) -> io::Result<MapType> {
        let mut maphdr = [0; 16];
        io.read_at(header.mapoffset, &mut maphdr)?;

        let maplength = read_be32(&maphdr[0..4]);
        let mut comprmap = vec![0; maplength as usize];
        io.read_exact(comprmap.as_mut_slice())?;

        Ok(Box::new(Self::decompress(header, &maphdr, &comprmap)?))
    }

    fn decompress(header: &Header, maphdr: &[u8], comprmap: &[u8]) -> io::Result<Self> {
        let hunkcount = header.hunkcount as usize;
        let hunkbytes = header.hunkbytes;
        let unitbytes = header.unitbytes;

        let mut bits = BitReader::new(&comprmap);
        let mut huffman = Huffman::new(16, 8);
        huffman.import_tree_rle(&mut bits)?;

        let mut map = vec![0; Self::offset(hunkcount)];

        // first decode the compression types
        let mut lastcomp = 0; // last known compression value
        let mut repcount = 0; // number of value repeats
        for hunknum in 0..hunkcount {
            if repcount > 0 {
                repcount -= 1;
            } else {
                match huffman.decode_one(&mut bits) as u8 {
                    COMPRESSION_RLE_SMALL => {
                        repcount = 2 + huffman.decode_one(&mut bits);
                    }
                    COMPRESSION_RLE_LARGE => {
                        repcount = 2 + 16 + (huffman.decode_one(&mut bits) << 4);
                        repcount += huffman.decode_one(&mut bits);
                    }
                    val => {
                        lastcomp = val;
                    }
                }
            }
            map[Self::offset(hunknum)] = lastcomp;
        }

        // then iterate through the hunks and extract the needed data
        let lengthbits = Self::bit_length(maphdr[12])?;
        let hunkbits = Self::bit_length(maphdr[13])?;
        let parentbits = Self::bit_length(maphdr[14])?;

        let mut curoffset = read_be48(&maphdr[4..10]);
        let mut lastself = 0;
        let mut lastparent = 0;
        for hunknum in 0..hunkcount {
            let mut offset = curoffset;
            let mut length = 0;
            let mut crc = 0;
            let mapentry = &mut map[Self::offset(hunknum)..Self::offset(hunknum + 1)];
            let compression = &mut mapentry[0];
            match *compression {
                // base types
                COMPRESSION_TYPE_0 | COMPRESSION_TYPE_1 | COMPRESSION_TYPE_2
                | COMPRESSION_TYPE_3 => {
                    length = bits.read(lengthbits);
                    curoffset += length as u64;
                    crc = bits.read(16) as u16;
                }
                COMPRESSION_NONE => {
                    length = hunkbytes;
                    curoffset += length as u64;
                    crc = bits.read(16) as u16;
                }
                COMPRESSION_SELF => {
                    offset = bits.read(hunkbits) as u64;
                    lastself = offset;
                }
                COMPRESSION_PARENT => {
                    offset = bits.read(parentbits as usize) as u64;
                    lastparent = offset;
                }
                // pseudo-types; convert into base types
                COMPRESSION_SELF_0 | COMPRESSION_SELF_1 => {
                    lastself += (*compression - COMPRESSION_SELF_0) as u64;
                    offset = lastself;
                    *compression = COMPRESSION_SELF;
                }
                COMPRESSION_PARENT_SELF => {
                    lastparent = ((hunknum as u64) * (hunkbytes as u64)) / (unitbytes as u64);
                    offset = lastparent;
                    *compression = COMPRESSION_PARENT;
                }
                COMPRESSION_PARENT_0 | COMPRESSION_PARENT_1 => {
                    if *compression == COMPRESSION_PARENT_1 {
                        lastparent += (hunkbytes / unitbytes) as u64;
                    }
                    offset = lastparent;
                    *compression = COMPRESSION_PARENT;
                }
                x => {
                    return Err(invalid_data(format!(
                        "chdv5: unknown hunk#{} compression type {}",
                        hunknum, x
                    )))
                }
            }
            write_be24(&mut mapentry[1..4], length);
            write_be48(&mut mapentry[4..10], offset);
            write_be16(&mut mapentry[10..12], crc);
        }
        let crc = read_be16(&maphdr[10..12]);
        let calc = crc16(&map);
        if calc != crc {
            return Err(invalid_data(format!(
                "chdv5: decompressed map crc {:04x} doesn't match header {:04x}",
                calc, crc
            )));
        }
        Ok(Self { map })
    }

    fn bit_length(val: u8) -> io::Result<usize> {
        match val {
            32..=u8::MAX => Err(invalid_data(format!(
                "chdv5: bit length {} is too big",
                val
            ))),
            val => Ok(val as usize),
        }
    }
}

impl Map for CompressedMap5 {
    fn locate(&self, hunknum: usize) -> u64 {
        let offset = Self::offset(hunknum);
        read_be48(&self.map[offset + 4..offset + 10])
    }
}

// read_hunk needs both Chd.io and Chd.cache mutable in Chd::read().
// to satisfy borrow checker have to move it into free function
fn read_hunk<T: R>(
    io: &mut T,
    map: &dyn Map,
    hunknum: usize,
    hunksize: usize,
    buf: &mut [u8],
) -> io::Result<()> {
    assert_eq!(buf.len(), hunksize);
    let offset = map.locate(hunknum);
    io.read_at(offset, buf)
}

pub struct Chd<T: R> {
    header: Header,
    filesize: u64,
    pos: i64,
    io: T,
    map: Box<dyn Map>,
    cache: Vec<u8>,   // cached data for reads not aligned to hunk boundaries
    cachehunk: usize, // cached hunk index
}

impl<T: R> Chd<T> {
    pub fn open(mut io: T) -> io::Result<Chd<T>> {
        let (header, map) = Header::read(&mut io)?;
        let filesize = io.seek(SeekFrom::End(0))?;
        let hunksize = header.hunkbytes as usize;
        let chd = Chd {
            header,
            filesize,
            pos: 0,
            io,
            map,
            cache: vec![0; hunksize],
            cachehunk: usize::MAX, // definitely out of any hunk index value
        };
        Ok(chd)
    }

    pub fn file_size(&self) -> u64 {
        self.filesize
    }

    pub fn version(&self) -> u32 {
        self.header.version
    }

    pub fn size(&self) -> u64 {
        self.header.size
    }

    pub fn hunk_size(&self) -> usize {
        self.header.hunkbytes as usize
    }

    pub fn hunk_size_u32(&self) -> u32 {
        self.header.hunkbytes
    }

    pub fn hunk_count(&self) -> u32 {
        self.header.hunkcount
    }

    pub fn unit_size(&self) -> usize {
        self.header.unitbytes as usize
    }

    pub fn unit_size_u32(&self) -> u32 {
        self.header.unitbytes
    }

    pub fn write_summary<W: Write>(&self, to: &mut W) -> io::Result<()> {
        writeln!(to, "File size: {}", self.file_size())?;
        writeln!(to, "CHD version: {}", self.version())?;
        writeln!(to, "Logical size: {}", self.size())?;
        writeln!(to, "Hunk Size: {}", self.hunk_size())?;
        writeln!(to, "Total Hunks: {}", self.hunk_count())?;
        writeln!(to, "Unit Size: {}", self.unit_size())?;
        write!(to, "Compression:")?;
        for i in 0..4 {
            match self.header.compressors[i] {
                0 => {
                    if i == 0 {
                        print!(" none");
                    }
                    break;
                }
                other => {
                    let mut s = String::with_capacity(5);
                    let mut v = other;
                    for _ in 0..4 {
                        let c = std::char::from_u32(v >> 24);
                        if c.is_some() && c.unwrap().is_ascii() {
                            s.push(c.unwrap());
                        } else {
                            s.push('?');
                        }
                        v <<= 8;
                    }
                    write!(to, " {} ({:08x})", s, other)?;
                }
            }
        }
        writeln!(to, "")?;
        let ratio = 1e2 * (self.file_size() as f32) / (self.size() as f32);
        writeln!(to, "Ratio: {:.1}%", ratio)?;
        write!(to, "SHA1: ")?;
        hex_writeln(to, &self.header.sha1)?;
        write!(to, "Data SHA1: ")?;
        hex_writeln(to, &self.header.rawsha1)?;
        for i in self.header.parentsha1.iter() {
            if *i > 0 {
                write!(to, "Parent SHA1: ")?;
                hex_writeln(to, &self.header.parentsha1)?;
                break;
            }
        }
        Ok(())
    }

    fn read_hunk(&mut self, hunknum: usize, buf: &mut [u8]) -> io::Result<()> {
        let hunksize = self.hunk_size();
        read_hunk(&mut self.io, &*self.map, hunknum, hunksize, buf)
    }
}

impl<T: R> Seek for Chd<T> {
    fn seek(&mut self, sf: SeekFrom) -> io::Result<u64> {
        let size = self.header.size as i64;
        let newpos = match sf {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::Current(x) => {
                if let Some(xx) = self.pos.checked_add(x) {
                    xx as i64
                } else {
                    return Err(invalid_data(format!(
                        "chd: overflowing seek {}{:+}, logical size {}",
                        self.pos,
                        x,
                        self.size()
                    )));
                }
            }
            SeekFrom::End(x) => {
                if let Some(xx) = size.checked_add(x) {
                    xx
                } else {
                    return Err(invalid_data(format!(
                        "chd: overflowing seek {} from logical end {}",
                        x,
                        self.size()
                    )));
                }
            }
        };
        if newpos < 0 || newpos > size {
            return Err(invalid_data(format!(
                "chd: invalid seek to {} out of logical size {}",
                newpos,
                self.size()
            )));
        }
        self.pos = newpos;
        Ok(self.pos as u64)
    }
}

impl<T: R> Read for Chd<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let hasbytes = self.header.size - self.pos as u64;
        if hasbytes == 0 {
            return Ok(0);
        }

        let mut dest = buf;
        if hasbytes < dest.len() as u64 {
            dest = dest.split_at_mut(hasbytes as usize).0;
        }
        let lastbyte = self.pos + dest.len() as i64 - 1;
        let hunkbytes = self.header.hunkbytes as usize;
        let hunkbytes64 = hunkbytes as i64;
        let hunklast = hunkbytes - 1;

        let first_hunk = (self.pos / hunkbytes64) as usize;
        let last_hunk = (lastbyte / hunkbytes64) as usize;
        let result = dest.len();

        // iterate over hunks
        for curhunk in first_hunk..=last_hunk {
            // determine start/end boundaries
            let startoffs = match curhunk == first_hunk {
                true => (self.pos % hunkbytes64) as usize,
                false => 0,
            };
            let endoffs = match curhunk == last_hunk {
                true => (lastbyte % hunkbytes64) as usize,
                false => hunklast as usize,
            };
            let length = endoffs + 1 - startoffs;
            let (mut head, tail) = dest.split_at_mut(length);
            dest = tail;

            if startoffs == 0 && endoffs == hunklast && curhunk != self.cachehunk {
                // if it's a full hunk, just read directly from disk unless it's the cached hunk
                self.read_hunk(curhunk, head)?;
            } else {
                // otherwise, read from the cache
                let hunksize = self.hunk_size();
                let cache = &mut self.cache;
                if curhunk != self.cachehunk {
                    // self.read_hunk(curhunk, cache)?; // error[E0499]: cannot borrow `*self` as mutable more than once at a time
                    read_hunk(&mut self.io, &mut *self.map, curhunk, hunksize, cache)?;
                    self.cachehunk = curhunk;
                }
                head.write(&cache[startoffs..startoffs + length])?;
            }
        }
        self.pos += result as i64;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    /*
    dd if=/dev/urandom of=data.bin bs=4096 count=8
    base64 < data.bin > data.b64
    du -b samples/data.b64
    */
    const DATA_SIZE: usize = 44267;

    fn open_chd(raw: &[u8]) -> Chd<Cursor<&[u8]>> {
        let file = Cursor::new(raw);
        let chd = Chd::open(file).unwrap();
        assert_eq!(chd.version(), V5);
        assert_eq!(chd.file_size(), raw.len() as u64);
        assert_eq!(chd.size(), DATA_SIZE as u64);
        chd
    }

    #[test]
    fn test_basic() {
        /*
        chdman createraw -hs 4096 -us 512 -i data.b64 -o none.chd -c none
        */
        let mut chd = open_chd(include_bytes!("../samples/none.chd"));

        // read hunk
        let mut buf = vec![0; chd.hunk_size()];
        chd.read_hunk(0, &mut buf).unwrap();
        let image = include_bytes!("../samples/data.b64");
        assert_eq!(buf, image[0..chd.hunk_size()]);

        // seek
        let last_byte = chd.size() - 1;
        assert_eq!(chd.seek(SeekFrom::Start(0)).unwrap(), 0);
        assert!(chd.seek(SeekFrom::Current(-1)).is_err());
        assert_eq!(chd.seek(SeekFrom::Current(0)).unwrap(), 0);
        assert_eq!(chd.seek(SeekFrom::Start(1)).unwrap(), 1);
        assert_eq!(chd.seek(SeekFrom::End(0)).unwrap(), chd.size());
        assert!(chd.seek(SeekFrom::Current(1)).is_err());
        assert_eq!(chd.seek(SeekFrom::Current(-1)).unwrap(), last_byte);
        assert!(chd.seek(SeekFrom::Current(0 - chd.size() as i64)).is_err());

        // read
        let hunksize = chd.hunk_size();
        let fixtures = [
            (1, 1),
            (0, hunksize),
            (0, hunksize - 1),
            (0, 1),
            (1, hunksize - 2),
            (hunksize - 1, 2),
            (hunksize - 1, hunksize + 2),
            (0, chd.size() as usize),
        ];
        for (offset, length) in fixtures.iter() {
            let end = *offset + *length;
            let original = &image[*offset..end];
            let mut sample = vec![0; *length];
            chd.read_at(*offset as u64, &mut sample).unwrap();
            assert_eq!(sample, original);
            // check read updates pos
            assert_eq!(chd.seek(SeekFrom::Current(0)).unwrap(), end as u64);
        }
    }

    #[test]
    fn test_huffman() {
        /*
        chdman createraw -hs 4096 -us 512 -i data.b64 -o huff.chd -c huff
        */
        open_chd(include_bytes!("../samples/huff.chd"));
    }
}
