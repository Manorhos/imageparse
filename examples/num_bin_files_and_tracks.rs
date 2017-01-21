extern crate imageparse;
extern crate env_logger;

use imageparse::Cuesheet;
use std::env;

fn main() {
    env_logger::init().unwrap();
    let filename = match env::args().nth(1) {
        Some(s) => s,
        None => panic!("No file supplied.")
    };
    let test = Cuesheet::open_cue(filename);
    if let Err(e) = test {
        println!("An error ocurred parsing the cue sheet: {:?}", e);
    } else if let Ok(res) = test {
        println!("Seems like everything worked fine! Number of bin files: {}", res.get_num_bin_files());
        println!("Number of tracks: {}", res.get_num_tracks());
    }
}
