extern crate inflate;

use super::Header;
use crate::bitstream::BitReader;
use crate::huffman::Huffman as HuffmanDecoder;
use crate::tags::*;
use crate::utils::{invalid_data, invalid_data_str};
use std::io;
use std::io::Write;

pub trait Decompress {
    fn decompress(&mut self, src: &[u8], dest: &mut [u8]) -> io::Result<()>;
}

pub type DecompressType = Option<Box<dyn Decompress>>;

fn create(header: &Header, tag: u32) -> DecompressType {
    match tag {
        0 => None,
        CHD_CODEC_HUFF => Some(Box::new(Huffman::new())),
        CHD_CODEC_ZLIB => Some(Box::new(Inflate::new())),
        x => Some(Box::new(Unknown::new(x))),
    }
}

pub(super) fn init(header: &Header) -> [DecompressType; 4] {
    [
        create(header, header.compressors[0]),
        create(header, header.compressors[1]),
        create(header, header.compressors[2]),
        create(header, header.compressors[3]),
    ]
}

struct Unknown {
    tag: u32,
}

impl Unknown {
    pub fn new(tag: u32) -> Self {
        Self { tag }
    }
}

impl Decompress for Unknown {
    fn decompress(&mut self, _src: &[u8], _dest: &mut [u8]) -> io::Result<()> {
        Err(invalid_data(format!(
            "codec {} not implemented",
            tag_string(self.tag)
        )))
    }
}

pub struct Huffman {
    inner: HuffmanDecoder,
}

impl Huffman {
    pub fn new() -> Self {
        Self {
            inner: HuffmanDecoder::new(256, 16),
        }
    }
}

impl Decompress for Huffman {
    fn decompress(&mut self, src: &[u8], dest: &mut [u8]) -> io::Result<()> {
        let mut stream = BitReader::new(src);
        self.inner.import_tree_huffman(&mut stream)?;
        for byte in dest.iter_mut() {
            *byte = self.inner.decode_one(&mut stream) as u8;
        }
        match stream.overflow() {
            false => Ok(()),
            true => Err(invalid_data_str(
                "codec:huffman: not enough compressed data",
            )),
        }
    }
}

pub struct Inflate {}

impl Inflate {
    pub fn new() -> Self {
        Self {}
    }
}

impl Decompress for Inflate {
    fn decompress(&mut self, src: &[u8], dest: &mut [u8]) -> io::Result<()> {
        let mut inflate = inflate::InflateWriter::new(dest);
        inflate.write(&src)?;
        Ok(())
    }
}
