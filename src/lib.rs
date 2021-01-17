mod bitstream;
mod decompress;
mod huffman;
mod lzma;
pub mod tags;
pub mod utils;
use bitstream::BitReader;
use decompress::DecompressType;
use huffman::Huffman;
use tags::*;
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

// Hunk compression, offset in file and length
type MapHunk = (u8, u64, u32);

// Different drive versions have different map format
trait Map {
    fn locate(&self, hunknum: usize) -> MapHunk;
    // Different versions use different digest algorithm
    fn validate(&self, hunknum: usize, buf: &[u8]) -> io::Result<()>;
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
    fn locate(&self, hunknum: usize) -> MapHunk {
        let offs = Self::offset(hunknum);
        let offset = read_be32(&self.map[offs..offs + 4]) as u64;
        (
            COMPRESSION_NONE,
            offset * self.hunkbytes,
            self.hunkbytes as u32,
        )
    }

    fn validate(&self, _hunknum: usize, _buf: &[u8]) -> io::Result<()> {
        Err(invalid_data_str(
            "Uncompressed map has no checksum for hunk",
        ))
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
    fn locate(&self, hunknum: usize) -> MapHunk {
        let o = Self::offset(hunknum);
        (
            self.map[o],
            read_be48(&self.map[o + 4..o + 10]),
            read_be24(&self.map[o + 1..o + 4]),
        )
    }

    fn validate(&self, hunknum: usize, buf: &[u8]) -> io::Result<()> {
        let o = Self::offset(hunknum);
        let crc = read_be16(&self.map[o + 10..o + 12]);
        let calc = crc16(buf);
        match calc == crc {
            true => Ok(()),
            false => Err(invalid_data(format!(
                "hunk#{}: crc16 {:04x} doesn't match map {:04x}",
                hunknum, calc, crc
            ))),
        }
    }
}

fn decompress_hunk<T: R>(
    io: &mut T,
    maphunk: MapHunk,
    dindex: usize,
    decompress: &mut [DecompressType],
    buf: &mut [u8],
) -> io::Result<()> {
    let (compression, offset, length) = maphunk;
    let d = decompress[dindex]
        .as_deref_mut()
        .ok_or(invalid_data(format!(
            "hunk@{}: no decompressor #{} for {}",
            offset, dindex, compression
        )))?;
    let mut compbuf = vec![0; length as usize];
    io.read_at(offset, compbuf.as_mut_slice())?;
    d.decompress(&compbuf, buf)
}

fn deref_parent<T: R>(parent: &mut ParentType<T>, offset: u64) -> io::Result<&mut Chd<T>> {
    parent.as_deref_mut().ok_or(invalid_data(format!(
        "hunk@{}: requires parent chd",
        offset
    )))
}

fn read_hunk_at<T: R>(
    io: &mut T,
    map: &dyn Map,
    decompress: &mut [DecompressType],
    parent: &mut ParentType<T>,
    maphunk: MapHunk,
    hunksize: usize,
    buf: &mut [u8],
) -> io::Result<()> {
    let (compression, offset, _) = maphunk;
    match compression {
        COMPRESSION_NONE => io.read_at(offset, buf),
        COMPRESSION_SELF => read_hunk(io, map, decompress, parent, offset as usize, hunksize, buf),
        COMPRESSION_PARENT => {
            let parent_chd = deref_parent(parent, offset)?;
            let parent_offs = offset * parent_chd.unit_size_u64();
            // partial read is OK, last hunk in parent could be shorter than hunksize
            parent_chd.seek(SeekFrom::Start(parent_offs))?;
            parent_chd.read(buf)?;
            Ok(())
        }
        COMPRESSION_TYPE_0 | COMPRESSION_TYPE_1 | COMPRESSION_TYPE_2 | COMPRESSION_TYPE_3 => {
            let dindex = (compression - COMPRESSION_TYPE_0) as usize;
            decompress_hunk(io, maphunk, dindex, decompress, buf)
        }
        x => Err(invalid_data(format!(
            "hunk@{}: unsupported compression {}",
            offset, x
        ))),
    }
}

// read_hunk needs both Chd.io and Chd.cache mutable in Chd::read().
// to satisfy borrow checker have to move it into free function
fn read_hunk<T: R>(
    io: &mut T,
    map: &dyn Map,
    decompress: &mut [DecompressType],
    parent: &mut ParentType<T>,
    hunknum: usize,
    hunksize: usize,
    buf: &mut [u8],
) -> io::Result<()> {
    assert_eq!(buf.len(), hunksize);
    let maphunk = map.locate(hunknum);
    read_hunk_at(io, map, decompress, parent, maphunk, hunksize, buf)
}

type ParentType<T> = Option<Box<Chd<T>>>;

pub struct Chd<T: R> {
    header: Header,
    filesize: u64,
    pos: i64,
    io: T,
    map: Box<dyn Map>,
    decompress: [DecompressType; 4],
    cache: Vec<u8>,   // cached data for reads not aligned to hunk boundaries
    cachehunk: usize, // cached hunk index
    parent: ParentType<T>,
}

impl<T: R> Chd<T> {
    pub fn open(mut io: T) -> io::Result<Chd<T>> {
        let (header, map) = Header::read(&mut io)?;
        let decompress = decompress::init(&header);
        let filesize = io.seek(SeekFrom::End(0))?;
        let hunksize = header.hunkbytes as usize;
        let chd = Chd {
            header,
            filesize,
            pos: 0,
            io,
            map,
            decompress,
            cache: vec![0; hunksize],
            cachehunk: usize::MAX, // definitely out of any hunk index value
            parent: None,
        };
        Ok(chd)
    }

    pub fn set_parent(&mut self, parent: Chd<T>) -> io::Result<()> {
        if parent.header.sha1 != self.header.parentsha1 {
            return Err(invalid_data(format!(
                "wrong parent sha1 {}: need {}",
                hex_string(&parent.header.sha1),
                hex_string(&self.header.parentsha1)
            )));
        }
        self.parent = Some(Box::new(parent));
        Ok(())
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

    pub fn hunk_count(&self) -> usize {
        self.header.hunkcount as usize
    }

    pub fn hunk_count_u32(&self) -> u32 {
        self.header.hunkcount
    }

    pub fn unit_size(&self) -> usize {
        self.header.unitbytes as usize
    }

    pub fn unit_size_u32(&self) -> u32 {
        self.header.unitbytes
    }

    pub fn unit_size_u64(&self) -> u64 {
        self.header.unitbytes as u64
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
                tag => {
                    write!(to, " {}", tag_string(tag))?;
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

    pub fn validate_hunk(&mut self, hunknum: usize) -> io::Result<()> {
        if hunknum >= self.hunk_count() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "invalid hunk#{}: chd has {} hunks",
                    hunknum,
                    self.hunk_count()
                ),
            ));
        }
        let maphunk = self.map.locate(hunknum);
        match maphunk.0 {
            COMPRESSION_SELF => self.validate_hunk(maphunk.1 as usize),
            COMPRESSION_PARENT => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("hunk#{}: parent chd hunks has no checksum", hunknum),
            )),
            _ => {
                let mut buf = vec![0; self.hunk_size()];
                self.read_hunk(hunknum, &mut buf)?;
                self.map.validate(hunknum, &buf)
            }
        }
    }

    pub fn validate(&mut self) -> io::Result<()> {
        for i in 0..self.hunk_count() {
            self.validate_hunk(i)?;
        }
        Ok(())
    }

    fn read_hunk(&mut self, hunknum: usize, buf: &mut [u8]) -> io::Result<()> {
        let hunksize = self.hunk_size();
        read_hunk(
            &mut self.io,
            &*self.map,
            &mut self.decompress,
            &mut self.parent,
            hunknum,
            hunksize,
            buf,
        )
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
                    read_hunk(
                        &mut self.io,
                        &mut *self.map,
                        &mut self.decompress,
                        &mut self.parent,
                        curhunk,
                        hunksize,
                        cache,
                    )?;
                    self.cachehunk = curhunk;
                }
                head.write(&cache[startoffs..startoffs + length])?;
            }
        }
        self.pos += result as i64;
        Ok(result)
    }
}

#[cfg(feature = "write_nop")]
impl<T: R> Write for Chd<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // only advance file position
        let hasbytes = self.header.size - self.pos as u64;
        Ok(if hasbytes < buf.len() as u64 {
            hasbytes as usize
        } else {
            buf.len()
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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
    const IMAGE: &[u8] = include_bytes!("../samples/data.b64");

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
        let image = IMAGE;
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

    fn test_compressed_chd(raw: &[u8]) {
        let mut chd = open_chd(raw);
        // read hunk
        let mut buf = vec![0; chd.hunk_size()];
        chd.read_hunk(0, &mut buf).unwrap();

        chd.validate().unwrap();
    }

    #[test]
    fn test_huffman() {
        /*
        chdman createraw -hs 4096 -us 512 -i data.b64 -o huff.chd -c huff
        */
        test_compressed_chd(include_bytes!("../samples/huff.chd"))
    }

    #[test]
    fn test_flac() {
        /*
        chdman createraw -hs 4096 -us 512 -i data.b64 -o flac.chd -c flac
        */
        test_compressed_chd(include_bytes!("../samples/flac.chd"))
    }

    #[test]
    fn test_lzma() {
        /*
        chdman createraw -hs 4096 -us 512 -i data.b64 -o lzma.chd -c lzma
        */
        test_compressed_chd(include_bytes!("../samples/lzma.chd"))
    }

    #[test]
    fn test_zlib() {
        /*
        chdman createraw -hs 4096 -us 512 -i data.b64 -o zlib.chd -c zlib
        */
        test_compressed_chd(include_bytes!("../samples/zlib.chd"))
    }

    #[test]
    fn test_compression_self() {
        /* generate file with lots of repetitions
        tr \\000 A < /dev/zero | dd of=a.bin bs=44267 count=1
        chdman createraw -hs 4096 -us 512 -i a.bin -o self.chd -c huff
        */
        let mut chd = open_chd(include_bytes!("../samples/self.chd"));
        let mut buf = vec![1; chd.hunk_size()];
        chd.read_hunk(0, &mut buf).unwrap();
        chd.validate().unwrap();
    }

    #[test]
    fn test_child() {
        /* changes some hunks in source data otherwise we will have the same sha1 hash in parent and child
        cp data.b64 child.b64
        dd if=a.bin of=child.b64 bs=4096 count=4 seek=3 conv=notrunc
        chdman createraw -hs 4096 -us 512 -i child.b64 -o child.chd -op huff.chd -c huff
        */
        let mut chd = open_chd(include_bytes!("../samples/child.chd"));
        let mut buf = vec![1; chd.hunk_size()];
        assert!(chd.read_hunk(0, &mut buf).is_err());

        let wrong = open_chd(include_bytes!("../samples/self.chd"));
        assert!(chd.set_parent(wrong).is_err());

        let parent = open_chd(include_bytes!("../samples/huff.chd"));
        chd.set_parent(parent).unwrap();
        chd.read_hunk(0, &mut buf).unwrap();
        // can't validate() parent hunks, read chd instead
        assert!(chd.validate().is_err());
        let image = include_bytes!("../samples/child.b64");
        let mut sample = vec![0; image.len()];
        chd.read_at(0, &mut sample).unwrap();
        assert_eq!(sample, image);
    }

    #[cfg(feature = "write_nop")]
    #[test]
    fn test_write() {
        let mut chd = open_chd(include_bytes!("../samples/none.chd"));
        let buf = [42; 4096];
        chd.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(chd.write(&buf).unwrap(), buf.len());
        chd.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(chd.write(&buf).unwrap(), 0);
    }
}
