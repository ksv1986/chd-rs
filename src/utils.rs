extern crate crc16;

use super::R;
use std::fmt::Write as FmtWrite;
use std::io;
use std::io::{SeekFrom, Write};

pub fn read_be16(data: &[u8]) -> u16 {
    assert_eq!(data.len(), 2);
    (data[0] as u16) << 8 | data[1] as u16
}

pub fn read_be24(data: &[u8]) -> u32 {
    assert_eq!(data.len(), 3);
    (data[0] as u32) << 16 | (data[1] as u32) << 8 | data[2] as u32
}

pub fn read_be32(data: &[u8]) -> u32 {
    assert_eq!(data.len(), 4);
    (data[0] as u32) << 24 | (data[1] as u32) << 16 | (data[2] as u32) << 8 | data[3] as u32
}

pub fn read_be48(data: &[u8]) -> u64 {
    assert_eq!(data.len(), 6);
    (data[0] as u64) << 40
        | (data[1] as u64) << 32
        | (data[2] as u64) << 24
        | (data[3] as u64) << 16
        | (data[4] as u64) << 8
        | data[5] as u64
}

pub fn read_be64(data: &[u8]) -> u64 {
    assert_eq!(data.len(), 8);
    (data[0] as u64) << 56
        | (data[1] as u64) << 48
        | (data[2] as u64) << 40
        | (data[3] as u64) << 32
        | (data[4] as u64) << 24
        | (data[5] as u64) << 16
        | (data[6] as u64) << 8
        | data[7] as u64
}

pub fn write_le16(data: &mut [u8], val: u16) {
    data[0] = val as u8;
    data[1] = (val >> 8) as u8;
}

pub fn write_be16(data: &mut [u8], val: u16) {
    data[1] = val as u8;
    data[0] = (val >> 8) as u8;
}

pub fn write_be24(data: &mut [u8], val: u32) {
    data[2] = val as u8;
    data[1] = (val >> 8) as u8;
    data[0] = (val >> 16) as u8;
}

pub fn write_be32(data: &mut [u8], val: u32) {
    data[3] = val as u8;
    data[2] = (val >> 8) as u8;
    data[1] = (val >> 16) as u8;
    data[0] = (val >> 24) as u8;
}

pub fn write_be48(data: &mut [u8], val: u64) {
    data[5] = val as u8;
    data[4] = (val >> 8) as u8;
    data[3] = (val >> 16) as u8;
    data[2] = (val >> 24) as u8;
    data[1] = (val >> 32) as u8;
    data[0] = (val >> 40) as u8;
}

#[derive(Default)]
pub struct IoStat {
    pub iops: u64,
    pub bytes: u64,
    pub seek: u64,
}

#[derive(Default)]
pub struct CodecStat {
    pub iops: u64,
    pub compressed: u64,
    pub decompressed: u64,
}

#[derive(Default)]
pub struct Stat {
    pub read: IoStat, // reads from underlaying io
    pub chd: IoStat,  // reads from chd
    pub decompress: [CodecStat; 4],
}

pub trait ReadAt {
    fn read_at(&mut self, stat: &mut IoStat, offset: u64, data: &mut [u8]) -> io::Result<()>;
    fn read_with_stat(&mut self, stat: &mut IoStat, data: &mut [u8]) -> io::Result<()>;
}

impl<T: R> ReadAt for T {
    fn read_at(&mut self, stat: &mut IoStat, offset: u64, data: &mut [u8]) -> io::Result<()> {
        self.seek(SeekFrom::Start(offset))?;
        stat.seek += 1;
        self.read_with_stat(stat, data)
    }

    fn read_with_stat(&mut self, stat: &mut IoStat, data: &mut [u8]) -> io::Result<()> {
        self.read_exact(data)?;
        stat.iops += 1;
        stat.bytes += data.len() as u64;
        Ok(())
    }
}

pub fn invalid_data_str(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

pub fn invalid_data(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

pub fn copy_from(mut to: &mut [u8], from: &[u8]) -> usize {
    to.write(from).unwrap()
}

pub fn hex_write<W: Write>(to: &mut W, hash: &[u8]) -> io::Result<()> {
    for i in hash {
        write!(to, "{:02x}", i)?;
    }
    Ok(())
}

pub fn hex_writeln<W: Write>(to: &mut W, hash: &[u8]) -> io::Result<()> {
    hex_write(to, hash)?;
    writeln!(to, "")?;
    Ok(())
}

pub fn hex_string(hash: &[u8]) -> String {
    let mut s = String::with_capacity(2 * hash.len());
    for i in hash {
        write!(s, "{:02x}", i).unwrap();
    }
    s
}

pub fn crc16(data: &[u8]) -> u16 {
    crc16::State::<crc16::CCITT_FALSE>::calculate(data)
}
