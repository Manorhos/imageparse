extern crate memmap;
#[macro_use]
extern crate log;

mod msf_index;

use std::fmt;
use std::path::Path;
use std::error::Error;
use std::str;

use memmap::{Mmap, Protection};

pub use self::msf_index::{MsfIndex, MsfParseError, MsfOverflow};


#[derive(Debug)]
pub enum CueParseError {
    MsfParseError(MsfParseError),
    MsfOverflow(MsfOverflow),
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
    Utf8Error(str::Utf8Error),
    OutOfRange,
    NoLocationSet
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

impl From<MsfOverflow> for CueParseError {
    fn from(err: MsfOverflow) -> CueParseError {
        CueParseError::MsfOverflow(err)
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

#[derive(Copy, Clone)]
enum Pregap {
    Index00(MsfIndex),
    #[allow(unused)]
    Silence(MsfIndex)
}

#[derive(Clone)]
struct Track {
    track_type: TrackType,
    pregap: Option<Pregap>,
    indices: Vec<MsfIndex>,
    num_sectors: usize
}

impl Track {
    fn get_first_msf(&self) -> MsfIndex {
        if let Some(pregap) = self.pregap {
            if let Pregap::Index00(msf) = pregap {
                return msf;
            }
        }
        return self.indices.first().unwrap().clone();
    }
}

struct BinFile {
    file: Mmap,
    bin_mode: BinMode,
    tracks: Vec<Track>
}

impl BinFile {
    fn get_num_sectors(&self) -> usize {
        self.file.len() / 2352
    }

    // Checks whether the given tracks are valid, calculates their lengths and
    // adds them to the BinFile.
    fn finalize(&mut self, tracks: Vec<TempTrackMetadata>) -> Result<(), CueParseError> {
        for win in tracks.windows(2) {
            let ref current_track = win[0];
            let ref next_track = win[1];
            if current_track.indices.is_empty() || next_track.indices.is_empty() {
                return Err(CueParseError::TrackWithoutIndex01);
            }
            let length = (next_track.get_first_msf() - current_track.get_first_msf())?;
            let track = Track {
                track_type: current_track.track_type.clone(),
                pregap: current_track.pregap.clone(),
                indices: current_track.indices.clone(),
                num_sectors: length.to_sectors()
            };
            self.tracks.push(track);
        }
        let last_track_metadata = tracks.last().unwrap();
        let last_track_sectors = self.get_num_sectors()
                                 - last_track_metadata.get_first_msf().to_sectors();
        let last_track = Track {
            track_type: last_track_metadata.track_type.clone(),
            pregap: last_track_metadata.pregap.clone(),
            indices: last_track_metadata.indices.clone(),
            num_sectors: last_track_sectors
        };
        self.tracks.push(last_track);

        if self.tracks.is_empty() {
            return Err(CueParseError::NoTracks);
        }
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

fn parse_track_line(line: &str) -> Result<(TempTrackMetadata, u8), CueParseError> {
    let line = line.trim();
    let line_elems = line.split_whitespace().collect::<Vec<&str>>();
    if line_elems.len() < 3 {
        return Err(CueParseError::InvalidTrackLine);
    }
    let track_number = line_elems[1].parse()?;
    let track_type = TrackType::try_from_str(line_elems[2])?;
    let track = TempTrackMetadata {
        track_type: track_type,
        pregap: None,
        indices: Vec::new(),
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

struct TempTrackMetadata {
    track_type: TrackType,
    pregap: Option<Pregap>,
    indices: Vec<MsfIndex>
}

impl TempTrackMetadata {
    fn get_first_msf(&self) -> MsfIndex {
        if let Some(pregap) = self.pregap {
            if let Pregap::Index00(msf) = pregap {
                return msf;
            }
        }
        return self.indices.first().unwrap().clone();
    }
}

#[derive(PartialEq)]
pub enum Event {
    TrackChange
}

impl Cuesheet {
    pub fn open_cue<P>(path: P) -> Result<Cuesheet, CueParseError>
        where P: AsRef<Path>
    {
        let cue_file = Mmap::open_path(path.as_ref().clone(), Protection::Read)?;
        let cue_bytes = unsafe { cue_file.as_slice() } ;
        let cue_str = str::from_utf8(cue_bytes)?;

        let mut bin_files: Vec<BinFile> = Vec::new();
        let mut current_track_number = 0;
        let mut current_bin_file: Option<BinFile> = None;
        let mut current_track: Option<TempTrackMetadata> = None;
        let mut tracks_for_current_bin_file: Vec<TempTrackMetadata> = Vec::new();

        for line in cue_str.lines() {
            if let Some(command) = line.split_whitespace().next() {
                let cmd_uppercase = command.to_uppercase();
                match cmd_uppercase.as_str() {
                    "FILE" => {
                        if let Some(mut current_bin_file) = current_bin_file {
                            if let Some(track) = current_track {
                                tracks_for_current_bin_file.push(track);
                            }
                            current_track = None;
                            current_bin_file.finalize(tracks_for_current_bin_file)?;
                            tracks_for_current_bin_file = Vec::new();
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
                    "PERFORMER" | "TITLE" => {} // TODO
                    "PREGAP" => {
                        if let Some(ref mut track) = current_track {
                            if track.pregap.is_some() {
                                // FIXME: Maybe use a more descriptive error message?
                                return Err(CueParseError::InvalidIndexNumber);
                            }
                            // TODO: Parse the pregap line and remember the length of it,
                            // since it is not in the bin file and it must be considered
                            // for calculating the offset into the bin files from now on
                        } else {
                            return Err(CueParseError::PregapCommandWithoutTrack);
                        }
                    }
                    "INDEX" => {
                        if let Some(ref mut track) = current_track {
                            let (index_number, index) = parse_index_line(line)?;
                            if index_number == 0 {
                                if track.pregap.is_some() {
                                    // INDEX 00 is also a type of pregap, so we have two
                                    // pregaps at this point.
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
        if let Some(mut last_bin_file) = current_bin_file {
            if let Some(last_track) = current_track {
                tracks_for_current_bin_file.push(last_track);
            } else {
                return Err(CueParseError::NoTracks);
            }
            last_bin_file.finalize(tracks_for_current_bin_file)?;
            bin_files.push(last_bin_file);
        } else {
            return Err(CueParseError::NoBinFiles);
        }
        Ok(Cuesheet{
            bin_files: bin_files,
            location: None
        })
    }

    pub fn get_num_bin_files(&self) -> usize {
        self.bin_files.len()
    }

    pub fn get_num_tracks(&self) -> usize {
        std::iter::Sum::sum(self.bin_files.iter().map(|x| x.tracks.len()))
    }

    pub fn get_current_track(&self) -> Result<u8, CueParseError> {
        if let Some(ref loc) = self.location {
            Ok(loc.track_no as u8 + 1)
        } else {
            Err(CueParseError::NoLocationSet)
        }
    }

    pub fn get_current_track_local_msf(&self) -> Result<MsfIndex, CueParseError> {
        if let Some(ref loc) = self.location {
            let start_of_track = self.bin_files[loc.bin_file_no]
                                     .tracks[loc.track_in_bin]
                                     .get_first_msf();
            let bin_local_msf = MsfIndex::from_sectors(loc.local_sector)?;
            let mut result = (bin_local_msf - start_of_track)?;
            // HACK...
            if loc.bin_file_no == 0 && loc.track_no == 0 {
                result = (result + MsfIndex::new(0,2,0)?)?;
            }
            Ok(result)
        } else {
            Err(CueParseError::NoLocationSet)
        }
    }

    pub fn get_current_global_msf(&self) -> Result<MsfIndex, CueParseError> {
        if let Some(ref loc) = self.location {
            // HACK! I think we need to add the real amount of pregaps here
            debug!("{:?}", loc.global_msf);
            let global_msf = (loc.global_msf.clone() + MsfIndex::new(0,2,0).unwrap())?;
            debug!("before: {:?}, after: {:?}", loc.global_msf, global_msf);
            Ok(global_msf)
        } else {
            Err(CueParseError::NoLocationSet)
        }
    }

    pub fn get_current_track_type(&self) -> Option<TrackType> {
        if let Some(loc) = self.location {
            Some(self.bin_files[loc.bin_file_no].tracks[loc.track_in_bin].track_type)
        } else {
            None
        }
    }

    pub fn get_physical_track_start(&self, track: u8) -> Result<MsfIndex, CueParseError> {
        if track == 0 {
            let len = std::iter::Sum::sum(self.bin_files.iter().map(|x| x.get_num_sectors()));
            return Ok(MsfIndex::from_sectors(len)?);
        }
        let mut bin_pos_on_disc = 0;
        let mut tracks_skipped = 0;
        // Find correct bin file
        for bin in self.bin_files.iter() {
            if bin.tracks.len() >= (track - tracks_skipped) as usize {
                let track_in_bin = track as usize - tracks_skipped as usize - 1;

                let track_index_one = bin.tracks[track_in_bin].indices.first().unwrap().to_sectors();

                let pos_on_disc = bin_pos_on_disc + track_index_one;

                // HACK! I think we need to add the real amount of pregaps here
                let physical_msf = (MsfIndex::from_sectors(pos_on_disc)? +
                                    MsfIndex::new(0,2,0)?)?;
                return Ok(physical_msf);
            } else {
                tracks_skipped += bin.tracks.len() as u8;
                bin_pos_on_disc += bin.get_num_sectors();
            }
        }
        Err(CueParseError::OutOfRange)
    }

    // TODO: Change error type
    pub fn set_location(&mut self, msf: &MsfIndex) -> Result<(), CueParseError> {
        // HACK! I think we need to subtract the real amount of pregaps here
        let real_msf = (*msf - MsfIndex::new(0,2,0).unwrap()).unwrap();
        let sector_no = real_msf.to_sectors();
        debug!("Sector Number: {}", sector_no);

        let mut bin_pos_on_disc = 0;
        let mut track_no = 0;
        // Find correct bin file
        for (bin_i, bin) in self.bin_files.iter().enumerate() {
            if (bin_pos_on_disc + bin.get_num_sectors()) > sector_no {
                // Find correct track
                let mut track_pos_on_disc = bin_pos_on_disc;
                for (track_i, track) in bin.tracks.iter().enumerate() {
                    if (track_pos_on_disc + track.num_sectors) > sector_no {
                        let real_msf_in_sectors = real_msf.to_sectors();
                        self.location = Some(Location {
                            bin_file_no: bin_i,
                            track_no: track_no + track_i as u8,
                            track_in_bin: track_i,
                            global_msf: real_msf,
                            local_sector: (real_msf_in_sectors - bin_pos_on_disc),
                            sectors_left: track.num_sectors - (real_msf_in_sectors - track_pos_on_disc)
                        });
                        return Ok(());
                    } else {
                        track_pos_on_disc += track.num_sectors;
                    }
                }
            } else {
                bin_pos_on_disc += bin.get_num_sectors();
                track_no += bin.tracks.len() as u8;
            }
        }
        debug!("set_location: didn't find location");
        Err(CueParseError::OutOfRange)
    }

    pub fn set_location_to_track(&mut self, track: u8) -> Result<(), CueParseError> {
        if track == 0 {
            return Err(CueParseError::OutOfRange);
        }
        let mut bin_pos_on_disc = 0;
        let mut tracks_skipped = 0;
        // Find correct bin file
        for (bin_i, bin) in self.bin_files.iter().enumerate() {
            if bin.tracks.len() >= (track - tracks_skipped) as usize {
                let track_in_bin = track as usize - tracks_skipped as usize - 1;

                let track_start = bin.tracks[track_in_bin].get_first_msf().to_sectors();
                let track_index_one = bin.tracks[track_in_bin].indices.first().unwrap().to_sectors();

                let pos_on_disc = bin_pos_on_disc + track_index_one;
                self.location = Some(Location {
                    bin_file_no: bin_i,
                    track_no: track - 1,
                    track_in_bin: track_in_bin,
                    global_msf: MsfIndex::from_sectors(pos_on_disc)?,
                    local_sector: track_index_one,
                    sectors_left: bin.tracks[track_in_bin].num_sectors -
                                  (track_index_one - track_start)
                });
                return Ok(());
            } else {
                tracks_skipped += bin.tracks.len() as u8;
                bin_pos_on_disc += bin.get_num_sectors();
            }
        }
        Err(CueParseError::OutOfRange)
    }

    pub fn get_next_sector(&mut self) -> Result<(&[u8], Option<Event>), CueParseError> {
        if self.location.is_none() {
            return Err(CueParseError::NoLocationSet);
        }

        let mut event = None;

        let current_bin_file;
        let current_sector;
        {
            let mut loc = self.location.as_mut().unwrap();
            current_bin_file = loc.bin_file_no;
            current_sector = loc.local_sector;
            loc.local_sector += 1;
            assert!(loc.sectors_left > 0);
            loc.sectors_left -= 1;
            loc.global_msf = loc.global_msf.next()?;
        }
        let new_loc = self.location.as_ref().unwrap().clone();
        if new_loc.sectors_left == 0 {
            // HACK! I think we need to add the real amount of pregaps here
            let new_physical_msf = (new_loc.global_msf + MsfIndex::new(0,2,0)?)?;
            self.set_location(&new_physical_msf)?;
            event = Some(Event::TrackChange);
        }

        let offset = current_sector * 2352;
        unsafe {
            Ok((&self.bin_files[current_bin_file].file.as_slice()[offset..offset + 2352],
               event))
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Location {
    bin_file_no: usize,
    track_no: u8,
    track_in_bin: usize,
    global_msf: MsfIndex,

    // Sector number local to the current bin file
    local_sector: usize,

    // Number of sectors left until the track changes
    sectors_left: usize
}


#[cfg(test)]
mod tests {
}
