extern crate claxon;
extern crate inflate;

use super::Header;
use crate::bitstream::BitReader;
use crate::cd;
use crate::ecc;
use crate::huffman::Huffman as HuffmanDecoder;
use crate::lzma::*;
use crate::tags::*;
use crate::utils::*;
use claxon::frame::{Block, FrameReader};
use std::io;
use std::io::{Cursor, Write};

pub trait Decompress {
    fn decompress(&mut self, src: &[u8], dest: &mut [u8]) -> io::Result<()>;
}

pub type DecompressType = Option<Box<dyn Decompress>>;

fn create(header: &Header, tag: u32) -> DecompressType {
    match tag {
        0 => None,
        CHD_CODEC_HUFF => Some(Box::new(Huffman::new())),
        CHD_CODEC_FLAC => Some(Box::new(Flac::new())),
        CHD_CODEC_LZMA => Some(Box::new(Lzma::new(header.hunkbytes).unwrap())),
        CHD_CODEC_ZLIB => Some(Box::new(Inflate::new())),
        CHD_CODEC_CD_FLAC => Some(Box::new(CdFlac::new(header.hunkbytes))),
        CHD_CODEC_CD_LZMA => Some(Box::new(CdDecompress::construct(
            Lzma::new(header.hunkbytes).unwrap(),
            Inflate::new(),
            header.hunkbytes,
        ))),
        CHD_CODEC_CD_ZLIB => Some(Box::new(CdDecompress::construct(
            Inflate::new(),
            Inflate::new(),
            header.hunkbytes,
        ))),
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

pub struct Lzma {
    handle: usize,
}

impl Lzma {
    pub fn new(hunkbytes: u32) -> io::Result<Self> {
        let handle = unsafe { lzma_create(hunkbytes) };
        match handle {
            0 => Err(invalid_data_str("failed to create lzma decoder")),
            _ => Ok(Self { handle }),
        }
    }
}

impl Drop for Lzma {
    fn drop(&mut self) {
        unsafe { lzma_destroy(self.handle) };
    }
}

impl Decompress for Lzma {
    fn decompress(&mut self, src: &[u8], dest: &mut [u8]) -> io::Result<()> {
        let error = unsafe {
            let srclen = src.len() as u32;
            let psrc = src.as_ptr();
            let dstlen = dest.len() as u32;
            let pdst = dest.as_mut_ptr();
            lzma_decompress(self.handle, psrc, srclen, pdst, dstlen)
        };
        match error {
            0 => Ok(()),
            _ => Err(invalid_data_str("lzma decompression failed")),
        }
    }
}

pub struct Flac {}

impl Flac {
    pub const SAMPLE_SIZE: usize = 4; // 16bit stereo

    pub fn new() -> Self {
        Self {}
    }
}

// buffer is moved into resulting block
fn flac_decompress(src: &[u8], buffer: Vec<i32>) -> io::Result<(Block, usize)> {
    let input = Cursor::new(src);
    let mut frame_reader = FrameReader::new(input);
    let result = frame_reader
        .read_next_or_eof(buffer)
        .map_err(|_| invalid_data_str("flac: failed to decode frame"))?;
    let block = result.ok_or(invalid_data_str("flac: data is too short"))?;
    if block.channels() != 2 {
        return Err(invalid_data(format!(
            "flac: expected stereo, but got {} channel samples",
            block.channels()
        )));
    }
    Ok((block, frame_reader.into_inner().position() as usize))
}

impl Decompress for Flac {
    fn decompress(&mut self, src: &[u8], dest: &mut [u8]) -> io::Result<()> {
        let write_endian = match src[0] {
            b'L' => write_le16,
            b'B' => write_be16,
            x => {
                return Err(invalid_data(format!(
                    "flac: invalid hunk endianness {:x}",
                    x
                )))
            }
        };
        let frame_size = Flac::SAMPLE_SIZE;
        let num_frames = dest.len() / frame_size;
        let buffer = vec![0; 2 * num_frames]; // 2 channe;s
        let block = flac_decompress(&src[1..], buffer)?.0;
        if block.duration() != num_frames as u32 {
            return Err(invalid_data(format!(
                "flac: decoded duration {} doesn't match number of frames in hunk {}",
                block.duration(),
                num_frames
            )));
        }
        for (i, (sl, sr)) in block.stereo_samples().enumerate() {
            write_endian(&mut dest[i * frame_size + 0..i * frame_size + 2], sl as u16);
            write_endian(&mut dest[i * frame_size + 2..i * frame_size + 4], sr as u16);
        }
        Ok(())
    }
}

struct CdDecompress<B: Decompress, S: Decompress> {
    base: B,
    subcode: S,
    buffer: Vec<u8>,
}

impl<B: Decompress, S: Decompress> CdDecompress<B, S> {
    fn construct(base: B, subcode: S, hunkbytes: u32) -> Self {
        Self {
            base,
            subcode,
            buffer: vec![0; hunkbytes as usize],
        }
    }
}

impl<B: Decompress, S: Decompress> Decompress for CdDecompress<B, S> {
    fn decompress(&mut self, src: &[u8], dest: &mut [u8]) -> io::Result<()> {
        let frames = dest.len() / cd::FRAME_SIZE;
        let ecc_bytes = (frames + 7) / 8;
        let (compr_start, compr_len) = if dest.len() <= u16::MAX as usize {
            (
                ecc_bytes + 2,
                read_be16(&src[ecc_bytes..ecc_bytes + 2]) as usize,
            )
        } else {
            (
                ecc_bytes + 3,
                read_be24(&src[ecc_bytes..ecc_bytes + 3]) as usize,
            )
        };

        let compr_end = compr_start + compr_len;
        let compressed = &src[compr_start..compr_end];
        let subcode = &src[compr_end..];
        let subcode_start = frames * cd::MAX_SECTOR_DATA;
        let subcode_end = subcode_start + frames * cd::MAX_SUBCODE_DATA;

        self.base
            .decompress(&compressed, &mut self.buffer[..subcode_start])?;
        self.subcode
            .decompress(&subcode, &mut self.buffer[subcode_start..subcode_end])?;

        // buffer contains first all frames data, then all frames subcode. reassemble frames
        for i in 0..frames {
            let frame_offs = i * cd::FRAME_SIZE;

            let data_offs = i * cd::MAX_SECTOR_DATA;
            let framedata = &mut dest[frame_offs..frame_offs + cd::MAX_SECTOR_DATA];
            copy_from(
                framedata,
                &self.buffer[data_offs..data_offs + framedata.len()],
            );

            let subcode_offs = subcode_start + i * cd::MAX_SUBCODE_DATA;
            let framesubcode =
                &mut dest[frame_offs + cd::MAX_SECTOR_DATA..frame_offs + cd::FRAME_SIZE];
            copy_from(
                framesubcode,
                &self.buffer[subcode_offs..subcode_offs + framesubcode.len()],
            );

            if src[i / 8] & (1 << (i % 8)) != 0 {
                let sector = &mut dest[frame_offs..frame_offs + cd::MAX_SECTOR_DATA];
                copy_from(sector, &cd::SYNC_HEADER);
                ecc::generate(sector);
            }
        }
        Ok(())
    }
}

struct CdFlac {
    buffer: Vec<u8>,
    inflate: Inflate,
}

impl CdFlac {
    const SAMPLE_PER_FRAME: usize = cd::MAX_SECTOR_DATA / Flac::SAMPLE_SIZE;

    pub fn new(hunkbytes32: u32) -> Self {
        let hunkbytes = hunkbytes32 as usize;
        assert!(hunkbytes % cd::FRAME_SIZE == 0);
        let num_frames = hunkbytes / cd::FRAME_SIZE;
        Self {
            buffer: vec![0; num_frames * cd::MAX_SUBCODE_DATA],
            inflate: Inflate::new(),
        }
    }
}

impl Decompress for CdFlac {
    fn decompress(&mut self, src: &[u8], dest: &mut [u8]) -> io::Result<()> {
        let mut src = src;
        let frames = dest.len() / cd::FRAME_SIZE;

        // first decompress flac data until all compressed samples are consumed
        let mut samples = frames * Self::SAMPLE_PER_FRAME;
        let mut sample_start = 0;
        while samples > 0 {
            let buffer = vec![0; 2 * samples]; // 2 channels
            let (block, pos) = flac_decompress(src, buffer)?;
            // in decoded block all samples are packed together. reassemble frames
            let decoded_samples = block.duration() as usize;
            for (i, (sl, sr)) in block.stereo_samples().enumerate() {
                let i = sample_start + i;
                let frame = i / Self::SAMPLE_PER_FRAME;
                let frame_offs = frame * cd::FRAME_SIZE;
                let sample_offs = frame_offs + (i % Self::SAMPLE_PER_FRAME) * Flac::SAMPLE_SIZE;
                write_be16(&mut dest[sample_offs + 0..sample_offs + 2], sl as u16);
                write_be16(&mut dest[sample_offs + 2..sample_offs + 4], sr as u16);
            }
            samples -= decoded_samples;
            src = &src[pos..];
            sample_start += decoded_samples;
        }
        // then decompress subcode data
        self.inflate.decompress(src, &mut self.buffer)?;
        for frame in 0..frames {
            let frame_offs = frame * cd::FRAME_SIZE;
            let subcode_offs = frame_offs + cd::MAX_SECTOR_DATA;
            let subcode = &mut dest[subcode_offs..subcode_offs + cd::MAX_SUBCODE_DATA];
            copy_from(
                subcode,
                &self.buffer[frame * cd::MAX_SUBCODE_DATA..(frame + 1) * cd::MAX_SUBCODE_DATA],
            );
        }
        Ok(())
    }
}
