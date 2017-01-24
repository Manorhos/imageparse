extern crate memmap;
#[macro_use]
extern crate log;

mod msf_index;

use std::fmt;
use std::path::Path;
use std::error::Error;
use std::str;

use memmap::{Mmap, Protection};

pub use self::msf_index::{MsfIndex, MsfParseError};


#[derive(Debug)]
pub enum CueParseError {
    MsfParseError(MsfParseError),
    ParseIntError(std::num::ParseIntError),
    IoError(std::io::Error),
    InvalidCommandError(String),
    InvalidTrackLine,
    InvalidTrackNumber,
    NoTracks,
    TrackWithoutIndex01,
    UnknownTrackType(String),
    UnknownBinMode(String),
    InvalidPregapLine,
    InvalidIndexLine,
    InvalidIndexNumber,
    NoBinFiles,
    FileNameParseError,
    TrackCommandWithoutBinFile,
    PregapCommandWithoutTrack,
    IndexCommandWithoutTrack,
    Utf8Error(str::Utf8Error)
}


impl Error for CueParseError {
    fn description(&self) -> &str {
        "Could not parse Cuesheet"
    }

    fn cause(&self) -> Option<&Error> {
        use CueParseError::*;
        match *self {
            MsfParseError(ref inner_err) => Some(inner_err),
            IoError(ref inner_err) => Some(inner_err),
            _ => None
        }
    }
}

impl fmt::Display for CueParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use CueParseError::*;
        match *self {
            MsfParseError(ref e) => e.fmt(f),
            IoError(ref e) => e.fmt(f),
            InvalidCommandError(ref s) => write!(f, "Invalid Command: {}", s),
            UnknownTrackType(ref s) => write!(f, "Unknown Track Type: {}", s),
            UnknownBinMode(ref s) => write!(f, "Unknown Binary File Mode: {}", s),
            NoBinFiles => write!(f, "No image files referenced in the cue sheet"),
            _ => write!(f, "{}", self.description())
        }
    }
}

impl From<std::io::Error> for CueParseError {
    fn from(err: std::io::Error) -> CueParseError {
        CueParseError::IoError(err)
    }
}

impl From<std::num::ParseIntError> for CueParseError {
    fn from(err: std::num::ParseIntError) -> CueParseError {
        CueParseError::ParseIntError(err)
    }
}

impl From<str::Utf8Error> for CueParseError {
    fn from(err: str::Utf8Error) -> CueParseError {
        CueParseError::Utf8Error(err)
    }
}

impl From<MsfParseError> for CueParseError {
    fn from(err: MsfParseError) -> CueParseError {
        CueParseError::MsfParseError(err)
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum BinMode {
    Binary,
    Wave,
    Mp3,
    Aiff,
    Motorola
}

impl BinMode {
    fn try_from_str(s: &str) -> Result<BinMode, CueParseError> {
        use self::BinMode::*;
        let s_uppercase = s.trim().to_uppercase();
        match s_uppercase.as_str() {
            "BINARY" => Ok(Binary),
            "WAVE" => Ok(Wave),
            "MP3" => Ok(Mp3),
            "AIFF" => Ok(Aiff),
            "MOTOROLA" => Ok(Motorola),
            _ => Err(CueParseError::UnknownBinMode(s_uppercase.clone()))
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum TrackType {
    // 2352 Bytes User Data, 2352 Bytes Raw Data
    Audio,
    // 2048 Bytes User Data, 2352 Bytes Raw Data
    Mode1,
    // 2336 Bytes User Data, 2352 Bytes Raw Data
    Mode2
}

impl TrackType {
    fn try_from_str(s: &str) -> Result<TrackType, CueParseError> {
        use self::TrackType::*;
        let s_uppercase = s.trim().to_uppercase();
        match s_uppercase.as_str() {
            "AUDIO" => Ok(Audio),
            "MODE1" => Ok(Mode1),
            "MODE2" | "MODE2/2352" => Ok(Mode2),
            _ => Err(CueParseError::UnknownTrackType(s_uppercase.clone()))
        }
    }
}

#[derive(Clone)]
enum Pregap {
    Index00(MsfIndex),
    Silence(MsfIndex)
}

#[derive(Clone)]
struct Track {
    track_type: TrackType,
    pregap: Option<Pregap>,
    indices: Vec<MsfIndex>
}

struct BinFile {
    file: Mmap,
    bin_mode: BinMode,
    tracks: Vec<Track>
}

pub struct Cuesheet {
    bin_files: Vec<BinFile>
}

fn parse_file_line(line: &str, cue_dir: Option<&Path>) -> Result<BinFile, CueParseError> {
    let line = line.trim();
    let quote_matches = line.match_indices("\"").collect::<Vec<_>>();
    if quote_matches.len() != 2 {
        return Err(CueParseError::FileNameParseError);
    }
    // Extract the filename surrounded by quotes
    let bin_filename = &line[(quote_matches[0].0 + 1)..quote_matches[1].0];
    let bin_mode_str = line.rsplit(|c: char| c.is_whitespace()).next().unwrap();

    let file = if let Some(cue_dir) = cue_dir {
        Mmap::open_path(cue_dir.join(bin_filename), Protection::Read)?
    } else {
        Mmap::open_path(bin_filename, Protection::Read)?
    };
    if file.len() % 2352 != 0 {
        warn!("Size of file \"{}\" is not a multiple of 2352 bytes.", bin_filename);
    }
    Ok(BinFile {
        file: file,
        bin_mode: BinMode::try_from_str(bin_mode_str)?,
        tracks: Vec::new()
    })
}

fn parse_track_line(line: &str) -> Result<(Track, u8), CueParseError> {
    let line = line.trim();
    let line_elems = line.split_whitespace().collect::<Vec<&str>>();
    if line_elems.len() < 3 {
        return Err(CueParseError::InvalidTrackLine);
    }
    let track_number = line_elems[1].parse()?;
    let track_type = TrackType::try_from_str(line_elems[2])?;
    let track = Track {
        track_type: track_type,
        pregap: None,
        indices: Vec::new()
    };
    Ok((track, track_number))
}

fn parse_index_line(line: &str) -> Result<(MsfIndex, u8), CueParseError> {
    let line = line.trim();
    let line_elems = line.split_whitespace().collect::<Vec<&str>>();
    if line_elems.len() < 3 {
        return Err(CueParseError::InvalidIndexLine);
    }
    let index_number = line_elems[1].parse()?;
    let index = MsfIndex::try_from_str(line_elems[2])?;
    Ok((index, index_number))
}

fn parse_pregap_line(line: &str) -> Result<MsfIndex, CueParseError> {
    let line = line.trim();
    let line_elems = line.split_whitespace().collect::<Vec<&str>>();
    if line_elems.len() != 2 {
        return Err(CueParseError::InvalidPregapLine);
    }
    let index = MsfIndex::try_from_str(line_elems[1])?;
    Ok(index)
}

impl Cuesheet {
    pub fn open_cue<P>(path: P) -> Result<Cuesheet, CueParseError>
        where P: AsRef<Path>
    {
        let cue_file = Mmap::open_path(path.as_ref().clone(), Protection::Read)?;
        let cue_bytes = unsafe { cue_file.as_slice() } ;
        let cue_str = str::from_utf8(cue_bytes)?;

        let mut bin_files: Vec<BinFile> = Vec::new();
        let mut current_bin_file: Option<BinFile> = None;
        let mut current_track: Option<Track> = None;
        let mut current_track_number = 0;

        for line in cue_str.lines() {
            if let Some(command) = line.split_whitespace().next() {
                let cmd_uppercase = command.to_uppercase();
                match cmd_uppercase.as_str() {
                    "FILE" => {
                        if let Some(prev_bin_file) = current_bin_file {
                            bin_files.push(prev_bin_file);
                        }
                        current_bin_file = Some(parse_file_line(line, path.as_ref().parent())?);
                        if current_bin_file.as_ref().unwrap().bin_mode != BinMode::Binary {
                            warn!("No bin file modes apart from Binary supported yet.");
                        }
                    }
                    "TRACK" => {
                        if let Some(ref mut bin_file) = current_bin_file {
                            if let Some(prev_track) = current_track {
                                if prev_track.indices.len() == 0 {
                                    return Err(CueParseError::TrackWithoutIndex01);
                                }
                                bin_file.tracks.push(prev_track);
                            }
                            let (next_track, next_track_number) = parse_track_line(line)?;
                            if next_track_number != current_track_number + 1 {
                                return Err(CueParseError::InvalidTrackNumber);
                            }
                            current_track_number = next_track_number;
                            current_track = Some(next_track);
                            debug!("New track, number: {}, mode: {:?}",
                                current_track_number,
                                current_track.as_ref().unwrap().track_type);
                        } else {
                            return Err(CueParseError::TrackCommandWithoutBinFile);
                        }
                    }
                    "PERFORMER" | "TITLE" => {} // TODO
                    "PREGAP" => {
                        if let Some(ref mut track) = current_track {
                            if track.pregap.is_some() {
                                // FIXME: Maybe use a more descriptive error message?
                                return Err(CueParseError::InvalidIndexNumber);
                            }
                            track.pregap = Some(Pregap::Silence(parse_pregap_line(line)?));
                        } else {
                            return Err(CueParseError::PregapCommandWithoutTrack);
                        }
                    }
                    "INDEX" => {
                        if let Some(ref mut track) = current_track {
                            let (index, index_number) = parse_index_line(line)?;
                            if index_number == 0 {
                                if track.pregap.is_some() {
                                    // FIXME: Maybe use a more descriptive error message?
                                    return Err(CueParseError::InvalidIndexNumber);
                                }
                                track.pregap = Some(Pregap::Index00(index));
                            } else if index_number as usize != track.indices.len() + 1 {
                                return Err(CueParseError::InvalidIndexNumber);
                            } else {
                                track.indices.push(index);
                            }
                        } else {
                            return Err(CueParseError::IndexCommandWithoutTrack);
                        }
                    }
                    "REM" => {
                        // TODO: Get ReplayGain information if present
                    }
                    _ =>  {
                        return Err(CueParseError::InvalidCommandError(cmd_uppercase.clone()));
                    }
                }
            }
        }
        if let Some(last_bin_file) = current_bin_file {
            bin_files.push(last_bin_file);
        } else {
            return Err(CueParseError::NoBinFiles);
        }
        if let Some(last_track) = current_track {
            bin_files.last_mut().unwrap().tracks.push(last_track);
        } else {
            return Err(CueParseError::NoTracks);
        }
        Ok(Cuesheet{ bin_files: bin_files })
    }

    pub fn get_num_bin_files(&self) -> usize {
        self.bin_files.len()
    }

    pub fn get_num_tracks(&self) -> usize {
        std::iter::Sum::sum(self.bin_files.iter().map(|x| x.tracks.len()))
    }

    // Returns the sector's full 2352 bytes, regardless of the track type
    pub fn get_full_sector(&self, msf: &MsfIndex) -> &[u8] {
        // TODO: Make multi-bin compatible
        let offset = msf.to_offset();
        let slice = unsafe { &self.bin_files[0].file.as_slice()[offset..offset + 2352] };
        slice
    }
}


#[cfg(test)]
mod tests {
}