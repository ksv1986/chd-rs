# CHD

CHD is a compressed disk image format used by [MAME](https://www.mamedev.org/) and some other emulators.

## Supported features

* Read-only
* CHD v5
* Compressed and uncompressed v5 map
* Huffman, Zlib, LZMA, FLAC hunk compression
* Parent CHD support
* Implements [std::io::Read](https://doc.rust-lang.org/std/io/trait.Read.html) and [std::io::Seek](https://doc.rust-lang.org/std/io/trait.Seek.html) traits
* Implements [std::io::Write](https://doc.rust-lang.org/std/io/trait.Write.html) as nop (can be disabled by turning off "write_nop" feature

## License

MIT
