extern crate filebuffer;
#[macro_use]
extern crate log;

#[cfg(feature = "serde-support")]
#[macro_use]
extern crate serde_derive;
#[cfg(feature = "serde-support")]
extern crate serde;

mod index;

use std::fmt;
use std::path::Path;
use std::error::Error;
use std::str;

use filebuffer::{FileBuffer};

pub use self::index::{GlobalSectorNumber, LocalSectorNumber, MsfIndex, MsfParseError};


const READAHEAD_SECTORS: usize = 60 * 75;


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
            PregapNotSupported => write!(f, "PREGAP command not supported yet"),
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
    // The sector number is a direct translation from the MSF timestamp given in the cue file,
    // so it is local to the corresponding bin file.
    Index00(LocalSectorNumber),
}

#[derive(Clone)]
struct Track {
    track_type: TrackType,
    pregap: Option<Pregap>,

    // These are direct translations from the MSF timestamp given in the cue file,
    // so they are local to the corresponding bin file.
    indices: Vec<LocalSectorNumber>,
    num_sectors: usize
}

impl Track {
    fn get_physical_track_start(&self) -> LocalSectorNumber {
        if let Some(Pregap::Index00(sector_no)) = self.pregap {
            return sector_no;
        }
        return *self.indices.first().unwrap();
    }
}

struct BinFile {
    file: FileBuffer,
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
            let length = next_track.get_physical_track_start() - current_track.get_physical_track_start();
            let track = Track {
                track_type: current_track.track_type.clone(),
                pregap: current_track.pregap.clone(),
                indices: current_track.indices.clone(),
                num_sectors: length.0
            };
            self.tracks.push(track);
        }
        let last_track_metadata = tracks.last().unwrap();
        let last_track_sectors = self.get_num_sectors()
                                 - last_track_metadata.get_physical_track_start().0;
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
        FileBuffer::open(cue_dir.join(bin_filename))?
    } else {
        FileBuffer::open(bin_filename)?
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

    // These are direct translations from the MSF timestamp given in the cue file,
    // so they are local to the corresponding bin file.
    indices: Vec<LocalSectorNumber>
}

impl TempTrackMetadata {
    fn get_physical_track_start(&self) -> LocalSectorNumber {
        if let Some(Pregap::Index00(sector_no)) = self.pregap {
            return sector_no;
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
        let cue_file = FileBuffer::open(path.as_ref().clone())?;
        let cue_str = str::from_utf8(&cue_file)?;

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
                    "PREGAP" => error!("Ignoring PREGAP command (not yet implemented)"),
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
                                track.pregap = Some(Pregap::Index00(LocalSectorNumber(index.to_sector_number())));
                            } else if index_number as usize != track.indices.len() + 1 {
                                return Err(CueParseError::InvalidIndexNumber);
                            } else {
                                track.indices.push(LocalSectorNumber(index.to_sector_number()));
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

    // TODO: Currently only returns 0 or 1
    pub fn get_current_index(&self) -> Result<u8, CueParseError> {
        if let Some(ref loc) = self.location {
            let index_one = self.bin_files[loc.bin_file_no]
                                    .tracks[loc.track_in_bin]
                                    .indices.first().unwrap();
            if loc.local_sector >= *index_one {
                Ok(1)
            } else {
                Ok(0)
            }
        } else {
            Err(CueParseError::NoLocationSet)
        }
    }

    pub fn get_current_track_local_msf(&self) -> Result<MsfIndex, CueParseError> {
        if let Some(ref loc) = self.location {
            let start_of_track = self.bin_files[loc.bin_file_no]
                                     .tracks[loc.track_in_bin]
                                     .get_physical_track_start();
            let index_one = *self.bin_files[loc.bin_file_no]
                                .tracks[loc.track_in_bin]
                                .indices.first().unwrap() - start_of_track;
            let track_local = loc.local_sector - start_of_track;
            if track_local < index_one {
                // Negative MSFs are (100,0,0) - x
                let reference = LocalSectorNumber(100 * 60 * 75);
                let offset = index_one - track_local;
                Ok((reference - offset).to_msf_index()?)
            } else {
                Ok((track_local - index_one).to_msf_index()?)
            }
        } else {
            Err(CueParseError::NoLocationSet)
        }
    }

    pub fn get_current_global_msf(&self) -> Result<MsfIndex, CueParseError> {
        if let Some(ref loc) = self.location {
            // HACK! I think we need to add the real amount of pregaps here
            let global_msf = (loc.global_position + 150).to_msf_index()?;
            debug!("before: {:?}, after: {:?}", loc.global_position, global_msf);
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

    pub fn get_first_track_type(&self) -> TrackType {
        self.bin_files.first().unwrap().tracks.first().unwrap().track_type
    }

    pub fn get_track_start(&self, track: u8) -> Result<MsfIndex, CueParseError> {
        // Track 0: Special case for PlayStation, return length of whole disc
        // TODO: Make this less ugly?
        if track == 0 {
            // 150: Pregap of first track, not included in image
            let len = 150 + self.bin_files.iter().map(|x| x.get_num_sectors()).sum::<usize>();
            return Ok(GlobalSectorNumber(len).to_msf_index()?);
        }
        let mut bin_pos_on_disc = GlobalSectorNumber(0);
        let mut tracks_skipped = 0;
        // Find correct bin file
        for bin in self.bin_files.iter() {
            if bin.tracks.len() >= (track - tracks_skipped) as usize {
                let track_in_bin = track as usize - tracks_skipped as usize - 1;

                let track_index_one = bin.tracks[track_in_bin].indices.first().unwrap();

                // HACK! I think we need to add the real amount of pregaps here
                let pos_on_disc = bin_pos_on_disc + *track_index_one + 150;

                return Ok(pos_on_disc.to_msf_index()?);
            } else {
                tracks_skipped += bin.tracks.len() as u8;
                bin_pos_on_disc = bin_pos_on_disc + bin.get_num_sectors();
            }
        }
        Err(CueParseError::OutOfRange)
    }

    // TODO: Change error type
    pub fn set_location(&mut self, msf: &MsfIndex) -> Result<(), CueParseError> {
        // HACK! I think we need to subtract the real amount of pregaps here
        let sector_no = msf.to_sector_number();

        // Pregap of first track
        if sector_no < 150 {
            self.location = Some(Location {
                bin_file_no: 0,
                track_no: 0,
                track_in_bin: 0,
                global_position: GlobalSectorNumber(0),
                local_sector: LocalSectorNumber(0),
                first_track_pregap_sectors_left: Some(150 - sector_no),
                sectors_left: 0,
            });
            return Ok(());
        }

        let sector_no = GlobalSectorNumber(sector_no - 150);

        let mut bin_pos_on_disc = GlobalSectorNumber(0);
        let mut track_no = 0;
        // Find correct bin file
        for (bin_i, bin) in self.bin_files.iter().enumerate() {
            if (bin_pos_on_disc + bin.get_num_sectors()) > sector_no {
                // Find correct track
                let mut track_pos_on_disc = bin_pos_on_disc;
                for (track_i, track) in bin.tracks.iter().enumerate() {
                    if (track_pos_on_disc + track.num_sectors) > sector_no {
                        self.location = Some(Location {
                            bin_file_no: bin_i,
                            track_no: track_no + track_i as u8,
                            track_in_bin: track_i,
                            global_position: sector_no,
                            local_sector: LocalSectorNumber((sector_no - bin_pos_on_disc).0),
                            first_track_pregap_sectors_left: None,
                            sectors_left: track.num_sectors - (sector_no.0 - track_pos_on_disc.0)
                        });
                        let offset = self.location.unwrap().local_sector.to_byte_offset();
                        let buffer = &self.bin_files[bin_i].file;
                        let prefetch = std::cmp::min(self.location.unwrap().sectors_left, READAHEAD_SECTORS);
                        buffer.prefetch(offset, prefetch * 2352);
                        return Ok(());
                    } else {
                        track_pos_on_disc = track_pos_on_disc + track.num_sectors;
                    }
                }
            } else {
                bin_pos_on_disc = bin_pos_on_disc + bin.get_num_sectors();
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
        let mut bin_pos_on_disc = GlobalSectorNumber(0);
        let mut tracks_skipped = 0;
        // Find correct bin file
        for (bin_i, bin) in self.bin_files.iter().enumerate() {
            if bin.tracks.len() >= (track - tracks_skipped) as usize {
                let track_in_bin = track as usize - tracks_skipped as usize - 1;

                let track_start = bin.tracks[track_in_bin].get_physical_track_start();
                let track_index_one = bin.tracks[track_in_bin].indices.first().unwrap();

                let pos_on_disc = bin_pos_on_disc + *track_index_one;
                self.location = Some(Location {
                    bin_file_no: bin_i,
                    track_no: track - 1,
                    track_in_bin: track_in_bin,
                    global_position: pos_on_disc,
                    local_sector: *track_index_one,
                    first_track_pregap_sectors_left: None,
                    sectors_left: bin.tracks[track_in_bin].num_sectors -
                                  (*track_index_one - track_start).0
                });
                let offset = self.location.unwrap().local_sector.to_byte_offset();
                let buffer = &self.bin_files[bin_i].file;
                let prefetch = std::cmp::min(self.location.unwrap().sectors_left, READAHEAD_SECTORS);
                buffer.prefetch(offset, prefetch * 2352);
                return Ok(());
            } else {
                tracks_skipped += bin.tracks.len() as u8;
                bin_pos_on_disc = bin_pos_on_disc + bin.get_num_sectors();
            }
        }
        Err(CueParseError::OutOfRange)
    }

    pub fn get_next_sector(&mut self) -> Result<(&[u8], Option<Event>), CueParseError> {
        if let Some(ref mut loc) = self.location {
            if let Some(pregap_sectors) = loc.first_track_pregap_sectors_left {
                let new_pregap_sectors = pregap_sectors - 1;
                if new_pregap_sectors == 0 {
                    *loc = Location {
                        bin_file_no: 0,
                        track_no: 0,
                        track_in_bin: 0,
                        global_position: GlobalSectorNumber(0),
                        local_sector: LocalSectorNumber(0),
                        first_track_pregap_sectors_left: None,
                        sectors_left: self.bin_files[0].get_num_sectors(),
                    };
                } else {
                    loc.first_track_pregap_sectors_left = Some(new_pregap_sectors);
                }
                return Ok((&[0u8; 2352], None));
            }
        }
        if self.location.is_none() {
            return Err(CueParseError::NoLocationSet);
        }

        let mut event = None;

        let current_bin_file;
        let current_sector;
        {
            let loc = self.location.as_mut().unwrap();
            current_bin_file = loc.bin_file_no;
            current_sector = loc.local_sector;
            loc.local_sector = loc.local_sector + 1;
            assert!(loc.sectors_left > 0);
            loc.sectors_left -= 1;
            loc.global_position = loc.global_position + 1;
        }
        let new_loc = self.location.as_ref().unwrap().clone();
        let offset = current_sector.to_byte_offset();
        if new_loc.sectors_left == 0 {
            // HACK! I think we need to add the real amount of pregaps here
            let new_physical_position = new_loc.global_position + 150;
            self.set_location(&new_physical_position.to_msf_index()?)?;
            event = Some(Event::TrackChange);
        }

        let buffer = &self.bin_files[current_bin_file].file;
        Ok((&buffer[offset..offset + 2352], event))
    }
}

#[derive(Clone, Copy, Debug)]
struct Location {
    bin_file_no: usize,
    track_no: u8,
    track_in_bin: usize,

    global_position: GlobalSectorNumber,

    // Sector number local to the current bin file
    local_sector: LocalSectorNumber,

    // Number of sectors until the pregap of the first track ends.
    // Only is Some(_) if we are currently in the pregap of the first track.
    // Caution: `global_position`, `local_sector` and `sectors_left` are not valid during that time.
    first_track_pregap_sectors_left: Option<usize>,

    // Number of sectors left until the track changes
    sectors_left: usize
}


#[cfg(test)]
mod tests {
}
