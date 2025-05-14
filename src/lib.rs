pub mod cue;
#[cfg(feature = "chd")]
pub mod chd;
mod index;
mod sbi;

pub use self::index::{MsfIndex, MsfIndexError};

use std::path::Path;
#[cfg(feature = "chd")]
use std::{fs::File, io::Read};

use log::{debug, error, info, warn};

use thiserror::Error;


#[derive(Debug, Error)]
pub enum ImageError {
    #[error("Unsupported image format")]
    UnsupportedFormat,
    #[error(transparent)]
    CueError(#[from] cue::CueError),
    #[cfg(feature = "chd")]
    #[error(transparent)]
    ChdError(#[from] chd::ChdImageError),
    #[error(transparent)]
    MsfIndexError(#[from] MsfIndexError),
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error("Index out of range")]
    OutOfRange,
}

pub trait Image {
    fn num_tracks(&self) -> usize;
    fn current_subchannel_q_valid(&self) -> bool;
    fn current_track(&self) -> Result<u8, ImageError>;
    fn current_index(&self) -> Result<u8, ImageError>;
    fn current_track_local_msf(&self) -> Result<MsfIndex, ImageError>;
    fn current_global_msf(&self) -> Result<MsfIndex, ImageError>;
    fn current_track_type(&self) -> Result<TrackType, ImageError>;
    fn first_track_type(&self) -> TrackType;
    fn track_start(&self, track: u8) -> Result<MsfIndex, ImageError>;

    fn set_location(&mut self, target: MsfIndex) -> Result<(), ImageError>;
    fn set_location_to_track(&mut self, track: u8) -> Result<(), ImageError>;
    fn advance_position(&mut self) -> Result<Option<Event>, ImageError>;
    #[allow(unused)]
    fn advise_prefetch(&mut self, location: MsfIndex) {}

    /// `buf` is expected to be 2352 bytes long
    fn copy_current_sector(&mut self, buf: &mut[u8]) -> Result<(), ImageError>;
}

pub fn open_file<P>(path: P) -> Result<Box<dyn Image>, ImageError>
    where P: AsRef<Path>
{
    #[cfg(feature = "chd")] {
        let mut magic = [0u8; 8];
        File::open(path.as_ref())?.read_exact(&mut magic)?;
        if &magic == b"MComprHD" {
            return Ok(Box::new(chd::ChdImage::open(path.as_ref())?));
        }
    }

    if let Some(ext) = path.as_ref().extension() {
        if ext.to_string_lossy().to_lowercase() == "cue" {
            return Ok(Box::new(cue::Cuesheet::open(path)?));
        }
    }

    Err(ImageError::UnsupportedFormat)
}

pub fn track_sha1s<I>(image: &mut I) -> Result<Vec<[u8; 20]>, ImageError>
    where I: Image
{
    use sha1::{Sha1, Digest};
    let old_location = image.current_global_msf();

    let mut v = Vec::new();

    image.set_location(MsfIndex::new(0,2,0).unwrap())?;

    for _ in 0..image.num_tracks() {
        let mut hasher = Sha1::new();
        let mut sector_buf = [0u8; 2352];
        let mut event = None;
        while event != Some(Event::TrackChange) && event != Some(Event::EndOfDisc) {
            image.copy_current_sector(&mut sector_buf)?;
            hasher.update(&sector_buf);
            event = image.advance_position()?;
        }

        v.push(hasher.finalize().into());
    }

    if let Ok(loc) = old_location {
        if let Err(e) = image.set_location(loc) {
            error!("Failed to restore old location: {:?}", e);
        }
    }

    Ok(v)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrackType {
    // 2352 Bytes User Data, 2352 Bytes Raw Data
    Audio,
    // 2048 Bytes User Data, 2352 Bytes Raw Data
    Mode1,
    // 2336 Bytes User Data, 2352 Bytes Raw Data
    Mode2
}

#[derive(PartialEq)]
pub enum Event {
    TrackChange,
    EndOfDisc
}
