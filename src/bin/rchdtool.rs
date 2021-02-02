extern crate chd;

use std::fs::File;
use std::io;

use chd::Chd;

fn main() -> io::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .expect("Usage: rchdtool <chd-file>");
    println!("Input file: {:?}", path);
    let file = File::open(path)?;
    let mut chd = Chd::open(file)?;
    chd.write_summary(&mut std::io::stdout())?;
    chd.dump_metadata(&mut std::io::stdout())?;
    Ok(())
}
