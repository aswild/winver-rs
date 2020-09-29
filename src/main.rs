use std::process::exit;

use winver::{get_file_fixed_info, Version};

const DEFAULT_PATH: &str = r"C:\Program Files\Vim\vim82\gvim.exe";

fn print_one(path: &str) -> u32 {
    match get_file_fixed_info(path) {
        Ok(info) => {
            println!("{}: {}", path, Version::from(info.product_version));
            0
        }
        Err(e) => {
            eprintln!("ERROR: {}: {}", path, e);
            1
        }
    }
}

fn main() {
    let mut any = false;
    let mut err = 0;
    for path in std::env::args().skip(1) {
        any = true;
        err += print_one(&path);
    }

    if !any {
        err += print_one(DEFAULT_PATH);
    }
    if err != 0 {
        exit(1);
    }
}
