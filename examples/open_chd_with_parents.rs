use std::env;
use std::path::PathBuf;

use imageparse::Image;
use imageparse::chd::ChdImage;

fn main() {
    env_logger::init();

    if env::args().len() < 2 {
        println!("Testing tool that opens a CHD file with its parents recursively");
        println!("Usage: {} <CHD filename> (<list of possible CHD parents>)",
            env::current_exe().expect("Failed to get current exe name").file_name().unwrap().to_string_lossy());
        return;
    }

    let main_chd_path = PathBuf::from(env::args().nth(1).unwrap());
    let possible_parent_paths: Vec<PathBuf> = env::args().skip(2).map(|x| PathBuf::from(x)).collect();

    let chd = ChdImage::open_with_parent(main_chd_path, &possible_parent_paths);

    if let Ok(mut chd) = chd {
        println!("Successfully opened CHD!");
        println!("Tracks: {}", chd.num_tracks());

        let sha1s = chd.track_sha1s();

        if let Ok(ref sha1s) = sha1s {
            println!("SHA-1 hashes for tracks:");
            for (i, sha1) in sha1s.iter().enumerate() {
                print!("Track {}: ", i + 1);
                for x in sha1.iter() {
                    print!("{:x}", x);
                }
                println!("");
            }
        } else if let Err(e) = sha1s {
            println!("Error generating track SHA1s: {:?}", e);
        }

    } else if let Err(e) = chd {
        println!("Error opening CHD: {:?}", e);
    }


}