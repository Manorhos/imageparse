use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;
use std::io::Read;

use crate::index::{MsfIndex, MsfParseError};
use crate::debug;

use thiserror::Error;


#[derive(Debug, Error)]
pub enum SbiParseError {
    #[error(transparent)]
    MsfParseError(#[from] MsfParseError),
    #[error("I/O error")]
    IoError(#[from] std::io::Error),
    #[error("Invalid mode/format specified")]
    InvalidMode,
    #[error("Input file does not seem like an SBI file (Magic doesn't match)")]
    NotAnSbiFile,
}


pub fn load_sbi_file<P>(path: P) -> Result<BTreeSet<u32>, SbiParseError>
        where P: AsRef<Path>
{
    let mut sbi_file = File::open(path)?;
    let mut sbi_data = Vec::new();
    sbi_file.read_to_end(&mut sbi_data)?;

    if sbi_data.len() < 4 || &sbi_data[0..4] != b"SBI\0" {
        return Err(SbiParseError::NotAnSbiFile);
    }

    let mut invalid_subq_lbas = BTreeSet::new();

    let mut index = 4;
    while index + 3 < sbi_data.len() {
        let m = sbi_data[index];
        let s = sbi_data[index + 1];
        let f = sbi_data[index + 2];

        debug!("m: {}, s: {}, f: {}", m, s, f);
        let msf = MsfIndex::from_bcd_values(m, s, f)?;
        let lba = msf.to_lba();
        invalid_subq_lbas.insert(lba);

        let mode = sbi_data[index + 3];
        if mode == 1 {
            index += 4 + 10;
        } else if mode <= 3 {
            index += 4 + 3;
        } else {
            return Err(SbiParseError::InvalidMode);
        }
    }

    Ok(invalid_subq_lbas)
}