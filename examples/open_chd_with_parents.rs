use std::env;
use std::path::PathBuf;

use imageparse::Image;
use imageparse::chd::ChdImage;

fn main() {
    if env::args().len() < 2 {
        println!("Testing tool that opens a CHD file with its parents recursively");
        println!("Usage: {} <CHD filename> (<list of possible CHD parents>)",
            env::current_exe().expect("Failed to get current exe name").file_name().unwrap().to_string_lossy());
        return;
    }

    let main_chd_path = PathBuf::from(env::args().nth(1).unwrap());
    let possible_parent_paths: Vec<PathBuf> = env::args().skip(2).map(|x| PathBuf::from(x)).collect();

    let chd = ChdImage::open_with_parent(main_chd_path, &possible_parent_paths);

    if let Ok(chd) = chd {
        println!("Successfully opened CHD!");
        println!("Tracks: {}", chd.num_tracks());
    } else if let Err(e) = chd {
        println!("Error opening CHD: {:?}", e);
    }
}