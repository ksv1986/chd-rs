pub const MAX_SECTOR_DATA: usize = 2352;
pub const MAX_SUBCODE_DATA: usize = 96;
pub const FRAME_SIZE: usize = MAX_SECTOR_DATA + MAX_SUBCODE_DATA;
pub const SYNC_NUM_BYTES: usize = 12;
pub const SYNC_HEADER: [u8; SYNC_NUM_BYTES] = [
    0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00,
];
