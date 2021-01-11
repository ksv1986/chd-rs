pub mod utils;
use utils::*;

use std::convert::TryFrom;
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};

// Define constraints for underlaying Chd file I/O
pub trait R: Read + Seek {}
impl<T: Read + Seek> R for T {}

const V5: u32 = 5;

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
    fn read<T: R>(io: &mut T) -> io::Result<Self> {
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
                Ok(header)
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

pub struct Chd<T: R> {
    header: Header,
    filesize: u64,
    io: T,
}

impl<T: R> Chd<T> {
    pub fn open(mut io: T) -> io::Result<Chd<T>> {
        let header = Header::read(&mut io)?;
        let filesize = io.seek(SeekFrom::End(0))?;
        let chd = Chd {
            header,
            filesize,
            io,
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

    pub fn hunk_size(&self) -> u32 {
        self.header.hunkbytes
    }

    pub fn hunk_count(&self) -> u32 {
        self.header.hunkcount
    }

    pub fn unit_size(&self) -> u32 {
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

    #[test]
    fn test_basic() {
        /*
        chdman createraw -c none -i data.b64 -o none.chd -hs 4096 -us 512
        */
        let raw = include_bytes!("../samples/none.chd");
        let file = Cursor::new(raw);
        let chd = Chd::open(file).unwrap();
        assert_eq!(chd.version(), V5);
        assert_eq!(chd.file_size(), raw.len() as u64);
        assert_eq!(chd.size(), DATA_SIZE as u64);
    }
}
