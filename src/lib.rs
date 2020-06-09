#[macro_use]
extern crate log;

#[cfg(feature = "serde-support")]
#[macro_use]
extern crate serde_derive;
#[cfg(feature = "serde-support")]
extern crate serde;
extern crate vec_map;

mod index;

use std::fmt;
use std::io;
use std::io::Read;
use std::path::Path;
use std::error::Error;
use std::str;

use std::fs::File;
use std::io::{Seek, SeekFrom};

use vec_map::VecMap;

pub use self::index::{MsfIndex, MsfParseError};


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
    PregapNotSupported,
    IndexCommandWithoutTrack,
    Utf8Error(str::Utf8Error),
    OutOfRange,
    NoLocationSet
}


impl Error for CueParseError {
    fn cause(&self) -> Option<&dyn Error> {
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
            PregapNotSupported => write!(f, "PREGAP command not supported yet"),
            _ => write!(f, "Could not parse Cuesheet")
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrackType {
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
struct Track {
    track_type: TrackType,

    // local to bin file
    starting_lba: u32,

    num_sectors: u32,

    // These are direct translations from the MSF timestamp given in the cue file,
    // so they are local to the corresponding bin file.
    // Note: Valid tracks always have an index 1.
    indices: VecMap<u32>
}

impl Track {
    fn first_index_lba(&self) -> u32 {
        if let Some(lba) = self.indices.get(0) {
            *lba
        } else {
            *self.indices.get(1).unwrap()
        }
    }
}

struct BinFile {
    file: File,
    bin_mode: BinMode,
    tracks: Vec<Track>
}

impl BinFile {
    fn num_sectors(&self) -> io::Result<u32> {
        Ok(self.file.metadata()?.len() as u32 / 2352)
    }

    // Checks whether the given tracks are valid, calculates their lengths and
    // adds them to the BinFile.
    fn finalize_tracks(&mut self) -> Result<(), CueParseError> {
        if self.tracks.is_empty() {
            return Err(CueParseError::NoTracks);
        }
        for i in 0..self.tracks.len() - 1 {
            if !self.tracks[i].indices.contains_key(1) ||
               !self.tracks[i + 1].indices.contains_key(1)
            {
                return Err(CueParseError::TrackWithoutIndex01);
            }
            let length = self.tracks[i+1].first_index_lba() - self.tracks[i].first_index_lba();
            self.tracks[i].num_sectors = length;
            self.tracks[i + 1].starting_lba = self.tracks[i].starting_lba + length;
        }
        let bin_num_sectors = self.num_sectors()?;
        let last_track = self.tracks.last_mut().unwrap();
        last_track.num_sectors = bin_num_sectors - last_track.first_index_lba();
        Ok(())
    }
}

pub struct Cuesheet {
    bin_files: Vec<BinFile>,
    location: Option<Location>
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
        File::open(cue_dir.join(bin_filename))?
    } else {
        File::open(bin_filename)?
    };
    let len = file.metadata()?.len();
    if len % 2352 != 0 {
        warn!("Size of file \"{}\" is not a multiple of 2352 bytes.", bin_filename);
    }
    Ok(BinFile {
        file,
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
        track_type,
        starting_lba: 0,
        num_sectors: 0,
        indices: VecMap::new(),
    };
    Ok((track, track_number))
}

fn parse_index_line(line: &str) -> Result<(u8, MsfIndex), CueParseError> {
    let line = line.trim();
    let line_elems = line.split_whitespace().collect::<Vec<&str>>();
    if line_elems.len() < 3 {
        return Err(CueParseError::InvalidIndexLine);
    }
    let index_number = line_elems[1].parse()?;
    let index = MsfIndex::try_from_str(line_elems[2])?;
    Ok((index_number, index))
}

#[allow(unused)]
fn parse_pregap_line(line: &str) -> Result<MsfIndex, CueParseError> {
    let line = line.trim();
    let line_elems = line.split_whitespace().collect::<Vec<&str>>();
    if line_elems.len() != 2 {
        return Err(CueParseError::InvalidPregapLine);
    }
    let index = MsfIndex::try_from_str(line_elems[1])?;
    Ok(index)
}

#[derive(PartialEq)]
pub enum Event {
    TrackChange
}

impl Cuesheet {
    pub fn from_cue_file<P>(path: P) -> Result<Cuesheet, CueParseError>
        where P: AsRef<Path>
    {
        let mut cue_file = File::open(path.as_ref().clone())?;
        let mut cue_string = String::new();
        cue_file.read_to_string(&mut cue_string)?;

        let mut bin_files: Vec<BinFile> = Vec::new();
        let mut current_track_number = 0;
        let mut current_bin_file: Option<BinFile> = None;
        let mut current_track: Option<Track> = None;
        let mut tracks_for_current_bin_file: Vec<Track> = Vec::new();

        for line in cue_string.lines() {
            if let Some(command) = line.split_whitespace().next() {
                let cmd_uppercase = command.to_uppercase();
                match cmd_uppercase.as_str() {
                    "FILE" => {
                        if let Some(mut current_bin_file) = current_bin_file {
                            if let Some(track) = current_track {
                                tracks_for_current_bin_file.push(track);
                            }
                            current_track = None;
                            current_bin_file.tracks = tracks_for_current_bin_file;
                            tracks_for_current_bin_file = Vec::new();
                            current_bin_file.finalize_tracks()?;
                            bin_files.push(current_bin_file);
                        }
                        current_bin_file = Some(parse_file_line(line, path.as_ref().parent())?);
                        if current_bin_file.as_ref().unwrap().bin_mode != BinMode::Binary {
                            warn!("No bin file modes apart from Binary supported yet.");
                        }
                    }
                    "TRACK" => {
                        if current_bin_file.is_some() {
                            if let Some(track) = current_track {
                                tracks_for_current_bin_file.push(track);
                            }
                            let (track, track_number) = parse_track_line(line)?;
                            if track_number != current_track_number + 1 {
                                return Err(CueParseError::InvalidTrackNumber);
                            }
                            current_track_number = track_number;
                            current_track = Some(track);
                        } else {
                            return Err(CueParseError::TrackCommandWithoutBinFile);
                        }
                    }
                    "PREGAP" => error!("Ignoring PREGAP command (not yet implemented)"),
                    "INDEX" => {
                        if let Some(ref mut track) = current_track {
                            let (index_number, index) = parse_index_line(line)?;
                            if index_number == 0 {
                                if track.indices.contains_key(0) {
                                    // INDEX 00 is also a type of pregap, so we have two
                                    // pregaps at this point.
                                    // FIXME: Maybe use a more descriptive error message?
                                    return Err(CueParseError::InvalidIndexNumber);
                                }
                                track.indices.insert(0, index.to_lba());
                            } else {
                                track.indices.insert(index_number as usize, index.to_lba());
                            }
                        } else {
                            return Err(CueParseError::IndexCommandWithoutTrack);
                        }
                    }
                    "FLAGS" | "CDTEXTFILE" | "CATALOG" | "PERFORMER" | "TITLE" | "ISRC" => {} // TODO
                    "REM" => {
                        // TODO: Get ReplayGain information if present
                    }
                    _ =>  {
                        return Err(CueParseError::InvalidCommandError(cmd_uppercase.clone()));
                    }
                }
            }
        }
        if let Some(mut last_bin_file) = current_bin_file {
            if let Some(last_track) = current_track {
                tracks_for_current_bin_file.push(last_track);
            } else {
                return Err(CueParseError::NoTracks);
            }
            last_bin_file.tracks = tracks_for_current_bin_file;
            last_bin_file.finalize_tracks()?;
            bin_files.push(last_bin_file);
        } else {
            return Err(CueParseError::NoBinFiles);
        }
        Ok(Cuesheet{
            bin_files,
            location: None
        })
    }

    pub fn num_bin_files(&self) -> usize {
        self.bin_files.len()
    }

    pub fn num_tracks(&self) -> usize {
        std::iter::Sum::sum(self.bin_files.iter().map(|x| x.tracks.len()))
    }

    pub fn current_track(&self) -> Result<u8, CueParseError> {
        if let Some(ref loc) = self.location {
            let mut track_no = 0;
            for i in 1..=loc.bin_file_no {
                track_no += self.bin_files[i - 1].tracks.len() as u8;
            }
            track_no += loc.track_in_bin as u8;
            Ok(track_no + 1)
        } else {
            Err(CueParseError::NoLocationSet)
        }
    }

    // TODO: Currently only returns 0 or 1
    pub fn current_index(&self) -> Result<u8, CueParseError> {
        if let Some(ref loc) = self.location {
            let start_of_track = self.bin_files[loc.bin_file_no]
                                     .tracks[loc.track_in_bin]
                                     .starting_lba;
            let index_one = *self.bin_files[loc.bin_file_no]
                                .tracks[loc.track_in_bin]
                                .indices.get(1).unwrap() - start_of_track;
            if loc.bin_local_lba >= index_one {
                Ok(1)
            } else {
                Ok(0)
            }
        } else {
            Err(CueParseError::NoLocationSet)
        }
    }

    pub fn current_track_local_msf(&self) -> Result<MsfIndex, CueParseError> {
        if let Some(ref loc) = self.location {
            let start_of_track = self.bin_files[loc.bin_file_no]
                                     .tracks[loc.track_in_bin]
                                     .starting_lba;
            let index_one = *self.bin_files[loc.bin_file_no]
                                .tracks[loc.track_in_bin]
                                .indices.get(1).unwrap() - start_of_track;
            debug!("current_track_local_msf: \
                    start_of_track: {:?}, index_one: {:?}, loc.local_lba: {}",
                    start_of_track, index_one, loc.bin_local_lba);
            let track_local = loc.bin_local_lba - start_of_track;
            if track_local < index_one {
                // Negative MSFs are (100,0,0) - x
                let reference = 100 * 60 * 75;
                let offset = index_one - track_local;
                Ok(MsfIndex::from_lba(reference - offset)?)
            } else {
                Ok(MsfIndex::from_lba(track_local - index_one)?)
            }
        } else {
            Err(CueParseError::NoLocationSet)
        }
    }

    pub fn current_global_msf(&self) -> Result<MsfIndex, CueParseError> {
        if let Some(ref loc) = self.location {
            // HACK! I think we need to add the real amount of pregaps here
            let global_msf = MsfIndex::from_lba(loc.global_lba + 150)?;
            debug!("before: {:?}, after: {:?}", loc.global_lba, global_msf);
            Ok(global_msf)
        } else {
            Err(CueParseError::NoLocationSet)
        }
    }

    pub fn current_track_type(&self) -> Option<TrackType> {
        if let Some(loc) = self.location {
            Some(self.bin_files[loc.bin_file_no].tracks[loc.track_in_bin].track_type)
        } else {
            None
        }
    }

    pub fn first_track_type(&self) -> TrackType {
        self.bin_files.first().unwrap().tracks.first().unwrap().track_type
    }

    pub fn track_start(&self, track: u8) -> Result<MsfIndex, CueParseError> {
        // Track 0: Special case for PlayStation, return length of whole disc
        // TODO: Make this less ugly?
        if track == 0 {
            // 150: Pregap of first track, not included in image
            let mut len = 150;
            for bin_file in self.bin_files.iter() {
                len += bin_file.num_sectors()?;
            }
            return Ok(MsfIndex::from_lba(len)?);
        }
        let mut bin_pos_on_disc = 0;
        let mut tracks_skipped = 0;
        // Find correct bin file
        for bin in self.bin_files.iter() {
            if bin.tracks.len() >= (track - tracks_skipped) as usize {
                let track_in_bin = track as usize - tracks_skipped as usize - 1;

                let track_index_one = bin.tracks[track_in_bin].indices.get(1).unwrap();

                // HACK! I think we need to add the real amount of pregaps here
                let pos_on_disc = bin_pos_on_disc + *track_index_one + 150;

                return Ok(MsfIndex::from_lba(pos_on_disc)?);
            } else {
                tracks_skipped += bin.tracks.len() as u8;
                bin_pos_on_disc = bin_pos_on_disc + bin.num_sectors()?;
            }
        }
        Err(CueParseError::OutOfRange)
    }

    // TODO: Change error type
    pub fn set_location(&mut self, target: MsfIndex) -> Result<(), CueParseError> {
        // TODO: Subtract all pregaps not present in the bin files
        let target_lba = target.to_lba() - 150;

        let mut current_lba_left = target_lba;
        for (bin_file_no, bin_file) in self.bin_files.iter().enumerate() {
            let num_sectors_bin = bin_file.num_sectors()?;
            if num_sectors_bin > current_lba_left {
                let bin_offset = current_lba_left;
                for (track_no, track) in bin_file.tracks.iter().enumerate() {
                    if track.num_sectors > current_lba_left {
                        self.location = Some(Location {
                            bin_file_no,
                            track_in_bin: track_no,
                            global_lba: target_lba,
                            bin_local_lba: bin_offset,
                        });
                        debug!("set_location {:?}, result: {:?}", target, self.location);
                        return Ok(());
                    } else {
                        current_lba_left -= track.num_sectors;
                    }
                }
            } else {
                current_lba_left -= num_sectors_bin;
            }
        }
        Err(CueParseError::OutOfRange)
    }

    pub fn set_location_to_track(&mut self, track: u8) -> Result<(), CueParseError> {
        let track_start_loc = self.track_start(track)?;
        debug!("track_start_loc: {:?}", track_start_loc);
        self.set_location(track_start_loc)
    }

    pub fn advance_position(&mut self) -> Result<Option<Event>, CueParseError> {
        if let Some(ref mut loc) = self.location {
            let bin_file = &self.bin_files[loc.bin_file_no];
            let track = &bin_file.tracks[loc.track_in_bin];
            let track_end = track.starting_lba + track.num_sectors;
            loc.global_lba += 1;
            loc.bin_local_lba += 1;
            if loc.bin_local_lba < track_end {
                Ok(None)
            } else {
                if bin_file.tracks.len() > loc.track_in_bin + 1 {
                    loc.track_in_bin += 1;
                } else if self.bin_files.len() >= loc.bin_file_no {
                    loc.bin_file_no += 1;
                    loc.track_in_bin = 0;
                    loc.bin_local_lba = 0;
                } // TODO else end of disc?
                Ok(Some(Event::TrackChange))
            }
            // TODO start reading sector asynchronously
        } else {
            Err(CueParseError::NoLocationSet)
        }
    }

    // `buf` needs to be 2352 bytes long.
    pub fn copy_current_sector(&self, buf: &mut [u8]) -> Result<(), CueParseError> {
        if let Some(loc) = self.location {
            debug!("Reading sector {}, local {}", loc.global_lba, loc.bin_local_lba);
            let mut file = &self.bin_files[loc.bin_file_no].file;
            file.seek(SeekFrom::Start(loc.bin_local_lba as u64 * 2352))?;
            file.read_exact(buf)?;
            Ok(())
        } else {
            Err(CueParseError::NoLocationSet)
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Location {
    bin_file_no: usize,
    track_in_bin: usize,
    global_lba: u32,
    bin_local_lba: u32,
}


#[cfg(test)]
mod tests {
}
