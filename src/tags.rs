pub const CHD_CODEC_HUFF: u32 = make_tag(['h', 'u', 'f', 'f']);
pub const CHD_CODEC_FLAC: u32 = make_tag(['f', 'l', 'a', 'c']);
pub const CHD_CODEC_LZMA: u32 = make_tag(['l', 'z', 'm', 'a']);
pub const CHD_CODEC_ZLIB: u32 = make_tag(['z', 'l', 'i', 'b']);
pub const CHD_CODEC_CD_FLAC: u32 = make_tag(['c', 'd', 'f', 'l']);
pub const CHD_CODEC_CD_LZMA: u32 = make_tag(['c', 'd', 'l', 'z']);
pub const CHD_CODEC_CD_ZLIB: u32 = make_tag(['c', 'd', 'z', 'l']);

#[allow(dead_code)]
pub mod metadata {
    use super::make_tag;
    pub const HARD_DISK: u32 = make_tag(['G', 'D', 'D', 'D']);
    pub const HARD_DISK_IDENT: u32 = make_tag(['I', 'D', 'N', 'T']);
    pub const HARD_DISK_KEY: u32 = make_tag(['K', 'E', 'Y', ' ']);

    // pcmcia CIS information
    pub const PCMCIA_CIS: u32 = make_tag(['C', 'I', 'S', ' ']);

    // standard CD-ROM metadata
    pub const CDROM_OLD: u32 = make_tag(['C', 'H', 'C', 'D']);
    pub const CDROM_TRACK: u32 = make_tag(['C', 'H', 'T', 'R']);
    pub const CDROM_TRACK2: u32 = make_tag(['C', 'H', 'T', '2']);
    pub const GDROM_OLD: u32 = make_tag(['C', 'H', 'G', 'T']);
    pub const GDROM_TRACK: u32 = make_tag(['C', 'H', 'G', 'D']);

    // standard A/V metadata
    pub const AV: u32 = make_tag(['A', 'V', 'A', 'V']);
    // A/V laserdisc frame metadata
    pub const AV_LD: u32 = make_tag(['A', 'V', 'L', 'D']);
}

pub const fn make_tag(data: [char; 4]) -> u32 {
    (data[0] as u32) << 24 | (data[1] as u32) << 16 | (data[2] as u32) << 8 | data[3] as u32
}

pub fn tag_string(tag: u32) -> String {
    let mut s = String::with_capacity(5);
    let mut v = tag;
    for _ in 0..4 {
        let c = std::char::from_u32(v >> 24);
        if c.is_some() && c.unwrap().is_ascii() {
            s.push(c.unwrap());
        } else {
            s.push('?');
        }
        v <<= 8;
    }
    format!("{} ({:8x})", s, tag)
}
