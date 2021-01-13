pub const CHD_CODEC_HUFF: u32 = make_tag(['h', 'u', 'f', 'f']);
pub const CHD_CODEC_LZMA: u32 = make_tag(['l', 'z', 'm', 'a']);
pub const CHD_CODEC_ZLIB: u32 = make_tag(['z', 'l', 'i', 'b']);

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
