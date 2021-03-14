pub mod cue;
mod index;
mod sbi;

pub use self::index::{MsfIndex, MsfIndexError};

use std::path::Path;

use log::{debug, error, info, warn};

use thiserror::Error;


#[derive(Debug, Error)]
pub enum ImageError {
    #[error("Unsupported image format")]
    UnsupportedFormat,
    #[error(transparent)]
    CueError(#[from] cue::CueError),
    #[error(transparent)]
    MsfIndexError(#[from] MsfIndexError),
    #[error(transparent)]
    IoError(#[from] std::io::Error),
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

    /// `buf` is expected to be 2352 bytes long
    fn copy_current_sector(&self, buf: &mut[u8]) -> Result<(), ImageError>;
}

pub fn open_file<P>(path: P) -> Result<Box<dyn Image>, ImageError>
    where P: AsRef<Path>
{
    Ok(Box::new(cue::Cuesheet::from_cue_file(path)?))
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
