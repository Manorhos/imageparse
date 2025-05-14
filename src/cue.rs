use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::str;

use crate::{debug, error, info, warn};

use thiserror::Error;

use vec_map::VecMap;

use crate::index::{MsfIndex, MsfIndexError};
use crate::{Event, Image, ImageError, TrackType};


// TODO: Rework these, most of these aren't really useful for users of the
// crate I think...
#[derive(Debug, Error)]
pub enum CueError {
    #[error("Error parsing MSF index")]
    MsfParseError(#[from] MsfIndexError),
    #[error(transparent)]
    ParseIntError(#[from] std::num::ParseIntError),
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error("Invalid command in cuesheet")]
    InvalidCommandError(String),
    #[error("Invalid TRACK line in cuesheet")]
    InvalidTrackLine,
    #[error("Invalid track number in cuesheet")]
    InvalidTrackNumber,
    #[error("No tracks in cuesheet")]
    NoTracks,
    #[error("Track missing index 01 in cuesheet")]
    TrackWithoutIndex01,
    #[error("Unknown track type {0} in cuesheet")]
    UnknownTrackType(String),
    #[error("Unknown bin mode {0} in cuesheet")]
    UnknownBinMode(String),
    #[error("Invalid PREGAP line in cuesheet")]
    InvalidPregapLine,
    #[error("Invalid INDEX line in cuesheet")]
    InvalidIndexLine,
    #[error("Invalid index number in cuesheet")]
    InvalidIndexNumber,
    #[error("No bin files referenced in cuesheet")]
    NoBinFiles,
    #[error("Error parsing file name in cuesheet")]
    FileNameParseError,
    #[error("Unexpected TRACK command in cuesheet")]
    TrackCommandWithoutBinFile,
    #[error("Unexpected INDEX command in cuesheet")]
    IndexCommandWithoutTrack,
    #[error("Error parsing input as UTF-8")]
    Utf8Error(#[from] str::Utf8Error),
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
    fn try_from_str(s: &str) -> Result<BinMode, CueError> {
        use self::BinMode::*;
        let s_uppercase = s.trim().to_uppercase();
        match s_uppercase.as_str() {
            "BINARY" => Ok(Binary),
            "WAVE" => Ok(Wave),
            "MP3" => Ok(Mp3),
            "AIFF" => Ok(Aiff),
            "MOTOROLA" => Ok(Motorola),
            _ => Err(CueError::UnknownBinMode(s_uppercase.clone()))
        }
    }
}


impl TrackType {
    fn try_from_str(s: &str) -> Result<TrackType, CueError> {
        use self::TrackType::*;
        let s_uppercase = s.trim().to_uppercase();
        match s_uppercase.as_str() {
            "AUDIO" => Ok(Audio),
            "MODE1" => Ok(Mode1),
            "MODE2" | "MODE2/2352" => Ok(Mode2),
            _ => Err(CueError::UnknownTrackType(s_uppercase.clone()))
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
    fn finalize_tracks(&mut self) -> Result<(), CueError> {
        if self.tracks.is_empty() {
            return Err(CueError::NoTracks);
        }
        for i in 0..self.tracks.len() - 1 {
            if !self.tracks[i].indices.contains_key(1) ||
               !self.tracks[i + 1].indices.contains_key(1)
            {
                return Err(CueError::TrackWithoutIndex01);
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
    location: Location,
    invalid_subq_lbas: Option<BTreeSet<u32>>,
}

fn parse_file_line(line: &str, cue_dir: Option<&Path>) -> Result<BinFile, CueError> {
    let line = line.trim();
    let quote_matches = line.match_indices("\"").collect::<Vec<_>>();
    if quote_matches.len() != 2 {
        return Err(CueError::FileNameParseError);
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

fn parse_track_line(line: &str) -> Result<(Track, u8), CueError> {
    let line = line.trim();
    let line_elems = line.split_whitespace().collect::<Vec<&str>>();
    if line_elems.len() < 3 {
        return Err(CueError::InvalidTrackLine);
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

fn parse_index_line(line: &str) -> Result<(u8, MsfIndex), CueError> {
    let line = line.trim();
    let line_elems = line.split_whitespace().collect::<Vec<&str>>();
    if line_elems.len() < 3 {
        return Err(CueError::InvalidIndexLine);
    }
    let index_number = line_elems[1].parse()?;
    let index = MsfIndex::try_from_str(line_elems[2])?;
    Ok((index_number, index))
}

#[allow(unused)]
fn parse_pregap_line(line: &str) -> Result<MsfIndex, CueError> {
    let line = line.trim();
    let line_elems = line.split_whitespace().collect::<Vec<&str>>();
    if line_elems.len() != 2 {
        return Err(CueError::InvalidPregapLine);
    }
    let index = MsfIndex::try_from_str(line_elems[1])?;
    Ok(index)
}

impl Cuesheet {
    pub fn open<P>(path: P) -> Result<Cuesheet, CueError>
        where P: AsRef<Path>
    {
        Self::_open(path.as_ref())
    }

    pub fn _open(path: &Path) -> Result<Cuesheet, CueError> {
        let mut cue_file = File::open(path)?;
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
                        current_bin_file = Some(parse_file_line(line, path.parent())?);
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
                                return Err(CueError::InvalidTrackNumber);
                            }
                            current_track_number = track_number;
                            current_track = Some(track);
                        } else {
                            return Err(CueError::TrackCommandWithoutBinFile);
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
                                    return Err(CueError::InvalidIndexNumber);
                                }
                                track.indices.insert(0, index.to_lba());
                            } else {
                                track.indices.insert(index_number as usize, index.to_lba());
                            }
                        } else {
                            return Err(CueError::IndexCommandWithoutTrack);
                        }
                    }
                    "FLAGS" | "CDTEXTFILE" | "CATALOG" | "PERFORMER" | "TITLE" | "ISRC" => {} // TODO
                    "REM" => {
                        // TODO: Get ReplayGain information if present
                    }
                    _ =>  {
                        return Err(CueError::InvalidCommandError(cmd_uppercase.clone()));
                    }
                }
            }
        }
        if let Some(mut last_bin_file) = current_bin_file {
            if let Some(last_track) = current_track {
                tracks_for_current_bin_file.push(last_track);
            } else {
                return Err(CueError::NoTracks);
            }
            last_bin_file.tracks = tracks_for_current_bin_file;
            last_bin_file.finalize_tracks()?;
            bin_files.push(last_bin_file);
        } else {
            return Err(CueError::NoBinFiles);
        }

        let sbi_path = path.with_extension("sbi");
        let mut invalid_subq_lbas = None;
        if sbi_path.exists() {
            match crate::sbi::load_sbi_file(sbi_path) {
                Ok(set) => {
                    info!("Found and loaded SBI file");
                    invalid_subq_lbas = Some(set);
                }
                Err(e) => warn!("Failed to load SBI file: {}", e),
            }
        }

        Ok(Cuesheet{
            bin_files,
            location: Location::default(),
            invalid_subq_lbas,
        })
    }

}

impl Image for Cuesheet {
    fn num_tracks(&self) -> usize {
        std::iter::Sum::sum(self.bin_files.iter().map(|x| x.tracks.len()))
    }

    fn current_subchannel_q_valid(&self) -> bool {
        if let Some(ref invalid_subq_lbas) = self.invalid_subq_lbas {
            !invalid_subq_lbas.contains(&self.location.global_lba)
        } else {
            true
        }
    }

    fn current_track(&self) -> Result<u8, ImageError> {
        let mut track_no = 0;
        for i in 1..=self.location.bin_file_no {
            track_no += self.bin_files[i - 1].tracks.len() as u8;
        }
        track_no += self.location.track_in_bin as u8;
        Ok(track_no + 1)
    }

    // TODO: Currently only returns 0 or 1
    fn current_index(&self) -> Result<u8, ImageError> {
        let index_one = *self.bin_files[self.location.bin_file_no]
                            .tracks[self.location.track_in_bin]
                            .indices.get(1).unwrap();
        if self.location.bin_local_lba >= index_one {
            Ok(1)
        } else {
            Ok(0)
        }
    }

    fn current_track_local_msf(&self) -> Result<MsfIndex, ImageError> {
        let start_of_track = self.bin_files[self.location.bin_file_no]
                                    .tracks[self.location.track_in_bin]
                                    .starting_lba;
        let index_one = *self.bin_files[self.location.bin_file_no]
                            .tracks[self.location.track_in_bin]
                            .indices.get(1).unwrap() - start_of_track;
        debug!("current_track_local_msf: \
                start_of_track: {:?}, index_one: {:?}, loc.local_lba: {}",
                start_of_track, index_one, self.location.bin_local_lba);
        let track_local = self.location.bin_local_lba - start_of_track;
        if track_local < index_one {
            // Negative MSFs are (100,0,0) - x
            let reference = 100 * 60 * 75;
            let offset = index_one - track_local;
            Ok(MsfIndex::from_lba(reference - offset)?)
        } else {
            Ok(MsfIndex::from_lba(track_local - index_one)?)
        }
    }

    fn current_global_msf(&self) -> Result<MsfIndex, ImageError> {
        let global_msf = MsfIndex::from_lba(self.location.global_lba)?;
        debug!("before: {:?}, after: {:?}", self.location.global_lba, global_msf);
        Ok(global_msf)
    }

    fn current_track_type(&self) -> Result<TrackType, ImageError> {
        Ok(self.bin_files[self.location.bin_file_no]
               .tracks[self.location.track_in_bin]
               .track_type)
    }

    fn first_track_type(&self) -> TrackType {
        self.bin_files.first().unwrap().tracks.first().unwrap().track_type
    }

    fn track_start(&self, track: u8) -> Result<MsfIndex, ImageError> {
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
        Err(ImageError::OutOfRange)
    }

    // TODO: Change error type
    fn set_location(&mut self, target: MsfIndex) -> Result<(), ImageError> {
        // TODO: Subtract all pregaps not present in the bin files
        let target_lba = target.to_lba();

        // Hack for pregap of first track
        if target_lba < 150 {
            self.location = Location {
                bin_file_no: 0,
                track_in_bin: 0,
                global_lba: target_lba,
                bin_local_lba: 0,
            };
            return Ok(());
        }

        let mut current_lba_left = target_lba - 150;
        for (bin_file_no, bin_file) in self.bin_files.iter().enumerate() {
            let num_sectors_bin = bin_file.num_sectors()?;
            if num_sectors_bin > current_lba_left {
                let bin_offset = current_lba_left;
                for (track_no, track) in bin_file.tracks.iter().enumerate() {
                    if track.num_sectors > current_lba_left {
                        self.location = Location {
                            bin_file_no,
                            track_in_bin: track_no,
                            global_lba: target_lba,
                            bin_local_lba: bin_offset,
                        };
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
        Err(ImageError::OutOfRange)
    }

    fn set_location_to_track(&mut self, track: u8) -> Result<(), ImageError> {
        let track_start_loc = self.track_start(track)?;
        debug!("track_start_loc: {:?}", track_start_loc);
        self.set_location(track_start_loc)
    }

    fn advance_position(&mut self) -> Result<Option<Event>, ImageError> {
        if self.location.global_lba < 150 {
            // Just doing this should be okay as the correct bin file,
            // track and local LBA are selected for starting to read at sector
            // (0,2,0) when seeking to the first track's pregap in `set_location()`.
            self.location.global_lba += 1;
            return Ok(None);
        }
        let bin_file = &self.bin_files[self.location.bin_file_no];
        let track = &bin_file.tracks[self.location.track_in_bin];
        let track_end = track.starting_lba + track.num_sectors;
        self.location.global_lba += 1;
        self.location.bin_local_lba += 1;
        if self.location.bin_local_lba < track_end {
            Ok(None)
        } else {
            if bin_file.tracks.len() > self.location.track_in_bin + 1 {
                self.location.track_in_bin += 1;
                Ok(Some(Event::TrackChange))
            } else if self.bin_files.len() > self.location.bin_file_no + 1 {
                self.location.bin_file_no += 1;
                self.location.track_in_bin = 0;
                self.location.bin_local_lba = 0;
                Ok(Some(Event::TrackChange))
            } else {
                Ok(Some(Event::EndOfDisc))
            }
        }
        // TODO start reading sector asynchronously
    }

    // `buf` needs to be 2352 bytes long.
    fn copy_current_sector(&mut self, buf: &mut [u8]) -> Result<(), ImageError> {
        debug!("Reading sector {}, local {}", self.location.global_lba, self.location.bin_local_lba);
        if self.location.global_lba < 150 {
            for x in buf.iter_mut() {
                *x = 0;
            }
            return Ok(());
        }
        let mut file = &self.bin_files[self.location.bin_file_no].file;
        file.seek(SeekFrom::Start(self.location.bin_local_lba as u64 * 2352))?;
        file.read_exact(buf)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
struct Location {
    bin_file_no: usize,
    track_in_bin: usize,
    global_lba: u32,
    bin_local_lba: u32,
}

impl Default for Location {
    // Use first sector after the first track's pregap as the default
    fn default() -> Location {
        Location {
            bin_file_no: 0,
            track_in_bin: 0,
            global_lba: 150,
            bin_local_lba: 0
        }
    }
}
