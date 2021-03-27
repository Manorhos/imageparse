use crate::{Event, Image, ImageError, MsfIndex, TrackType};

use std::collections::BTreeSet;
use std::path::Path;

use chdr::{ChdError, ChdFile};
use chdr::metadata::CdTrackInfo;

use log::{debug, info, warn};

use thiserror::Error;


const BYTES_PER_SECTOR: u32 =  2352 + 96;

// TODO: Can we really assume that the first track's pregap is always
// two seconds long?
const FIRST_TRACK_PREGAP: u32 = 150;

#[derive(Debug)]
struct Track {
    start_lba: u32,
    track_type: TrackType,

    // Tracks are padded to multiples of 4 sectors in CHDs.
    // This is the number of padding sectors that has to be taken
    // into account when calculating the LBAs for a particular track.
    padding_offset: u32,
    track_info: CdTrackInfo,
}

#[derive(Debug, Error)]
pub enum ChdImageError {
    #[error(transparent)]
    ChdError(#[from] ChdError),
    #[error("CHD file does not seem like a CDROM image (wrong hunk size)")]
    WrongHunkSize,
    #[error("Wrong buffer size, needs to be 2352 bytes")]
    WrongBufferSize,
    #[error("Unsupported sector format: {0}")]
    UnsupportedSectorFormat(String)
}

pub struct ChdImage {
    chd: ChdFile,
    tracks: Vec<Track>,

    hunk: Vec<u8>,
    current_hunk_no: u32,
    current_lba: u32,
    // Starts counting from 0
    current_track: usize,

    sectors_per_hunk: u32,

    invalid_subq_lbas: Option<BTreeSet<u32>>,
}

impl ChdImage {
    pub fn open<P>(path: P) -> Result<ChdImage, ChdImageError>
        where P: AsRef<Path>
    {
        let mut chd = ChdFile::open(path.as_ref())?;

        if chd.hunk_len() % BYTES_PER_SECTOR != 0 {
            return Err(ChdImageError::WrongHunkSize);
        }

        let mut hunk = vec![0; chd.hunk_len() as usize];
        chd.read_hunk(0, &mut hunk[..])?;

        let sectors_per_hunk = chd.hunk_len() / BYTES_PER_SECTOR;

        let mut tracks = Vec::new();
        if let Ok(chd_tracks) = chd.cd_tracks() {
            let mut current_lba = FIRST_TRACK_PREGAP;
            let mut total_padding = 0;
            for chd_track in chd_tracks {
                let track_type = match chd_track.track_type.as_str() {
                    "MODE1_RAW" => TrackType::Mode1,
                    "MODE2_RAW" => TrackType::Mode2,
                    "AUDIO" => TrackType::Audio,
                    _ => return Err(ChdImageError::UnsupportedSectorFormat(chd_track.track_type)),
                };
                let start_lba = current_lba;
                current_lba += chd_track.frames;
                let padding_offset = total_padding;
                let align_remainder = chd_track.frames % 4;
                if align_remainder > 0 {
                    total_padding += 4 - align_remainder;
                }
                tracks.push(Track {
                    start_lba,
                    track_type,
                    padding_offset,
                    track_info: chd_track
                });
            }
        }

        let sbi_path = path.as_ref().with_extension("sbi");
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

        Ok(ChdImage {
            chd,
            hunk,
            current_hunk_no: 0,
            current_lba: 150,
            current_track: 0,

            sectors_per_hunk,

            tracks,

            invalid_subq_lbas
        })
    }

    fn update_current_track(&mut self, lba: u32) -> Result<(), ImageError> {
        let current_track = &self.tracks[self.current_track];
        let current_track_range_end = current_track.start_lba + current_track.track_info.frames;
        if !(current_track.start_lba..current_track_range_end).contains(&lba) {
            if let Some(index) = self.tracks.iter().position(
                |x| lba >= x.start_lba && lba < (x.start_lba + x.track_info.frames)
            ) {
                self.current_track = index;
                Ok(())
            } else {
                Err(ImageError::OutOfRange)
            }
        } else {
            Ok(())
        }
    }

    fn set_location_lba(&mut self, lba: u32) -> Result<(), ImageError> {
        self.current_lba = lba;
        // TODO: Can we really assume that the first track's pregap is always
        // two seconds long?
        if lba < FIRST_TRACK_PREGAP {
            self.current_track = 0;
            return Ok(());
        }

        self.update_current_track(lba)?;

        let current_track = &self.tracks[self.current_track];

        let lba = lba + current_track.padding_offset - FIRST_TRACK_PREGAP;
        debug!("set_location_lba {}", lba);
        let hunk_no = lba / self.sectors_per_hunk;
        if hunk_no > self.chd.num_hunks() {
            return Err(ImageError::OutOfRange);
        }
        if hunk_no != self.current_hunk_no {
            let res = self.chd.read_hunk(hunk_no, &mut self.hunk[..]);
            if let Err(e) = res {
                return Err(ChdImageError::from(e).into());
            }
            self.current_hunk_no = hunk_no;
        }
        Ok(())
    }
}

impl Image for ChdImage {
    fn num_tracks(&self) -> usize {
        self.tracks.len()
    }

    fn current_subchannel_q_valid(&self) -> bool {
        if let Some(ref invalid_subq_lbas) = self.invalid_subq_lbas {
            !invalid_subq_lbas.contains(&self.current_lba)
        } else {
            true
        }
    }

    fn current_track(&self) -> Result<u8, ImageError> {
        Ok(self.current_track as u8 + 1)
    }

    fn current_index(&self) -> Result<u8, ImageError> {
        let current_track = &self.tracks[self.current_track];
        let track_local_lba = self.current_lba - current_track.start_lba;
        let index = if track_local_lba > current_track.track_info.pregap.unwrap_or(0) {
            1
        } else {
            0
        };
        Ok(index)
    }

    fn current_track_local_msf(&self) -> Result<MsfIndex, ImageError> {
        let current_track = &self.tracks[self.current_track];
        let index01_lba =
            current_track.start_lba + current_track.track_info.pregap.unwrap_or(150);

        if self.current_lba < index01_lba {
            // Negative MSFs are (100,0,0) - x
            let reference = 100 * 60 * 75;
            let offset = index01_lba - self.current_lba;
            Ok(MsfIndex::from_lba(reference - offset)?)
        } else {
            Ok(MsfIndex::from_lba(self.current_lba - index01_lba)?)
        }
    }

    fn current_global_msf(&self) -> Result<MsfIndex, ImageError> {
        Ok(MsfIndex::from_lba(self.current_lba)?)
    }

    fn current_track_type(&self) -> Result<TrackType, ImageError> {
        let current_track = &self.tracks[self.current_track];
        Ok(current_track.track_type)
    }

    fn first_track_type(&self) -> TrackType {
        self.tracks.first().unwrap().track_type
    }

    fn track_start(&self, track: u8) -> Result<MsfIndex, ImageError> {
        // Track 0: Special case for PlayStation, return length of whole disc
        // TODO: Make this less ugly?
        if track == 0 {
            let len = self.chd.num_hunks() * self.chd.hunk_len();
            let num_sectors = FIRST_TRACK_PREGAP + len / BYTES_PER_SECTOR;
            Ok(MsfIndex::from_lba(num_sectors)?)
        } else if track <= self.tracks.len() as u8 {
            let track = &self.tracks[track as usize - 1];
            let start_lba_index01 =
                track.start_lba + track.track_info.pregap.unwrap_or(150);
            debug!("track_start: {:?} {:?}", track, MsfIndex::from_lba(start_lba_index01));
            Ok(MsfIndex::from_lba(start_lba_index01)?)
        } else {
            Err(ImageError::OutOfRange)
        }
    }

    fn set_location(&mut self, target: MsfIndex) -> Result<(), ImageError> {
        self.set_location_lba(target.to_lba())
    }

    fn set_location_to_track(&mut self, track: u8) -> Result<(), ImageError> {
        debug!("set_location_to_track {}", track);
        let track_start = self.track_start(track)?;
        self.set_location(track_start)?;
        Ok(())
    }

    fn advance_position(&mut self) -> Result<Option<Event>, ImageError> {
        let old_track = self.current_track;
        let res = self.set_location_lba(self.current_lba + 1);
        if let Err(e) = res {
            if let ImageError::OutOfRange = e {
                Ok(Some(Event::EndOfDisc))
            } else {
                Err(e)
            }
        } else if self.current_track != old_track {
            Ok(Some(Event::TrackChange))
        } else {
            Ok(None)
        }
    }

    fn copy_current_sector(&self, buf: &mut[u8]) -> Result<(), ImageError> {
        if buf.len() != 2352 {
            return Err(ChdImageError::WrongBufferSize.into())
        }
        if self.current_lba < FIRST_TRACK_PREGAP {
            buf.fill(0);
            return Ok(());
        }
        let current_track = &self.tracks[self.current_track];
        let current_file_lba = self.current_lba + current_track.padding_offset - FIRST_TRACK_PREGAP;
        let sector_in_hunk = current_file_lba % self.sectors_per_hunk;
        let sector_start = (sector_in_hunk * BYTES_PER_SECTOR) as usize;
        let sector = &self.hunk[sector_start..sector_start + 2352];
        buf.clone_from_slice(sector);

        if self.current_track_type().unwrap() == TrackType::Audio {
            for x in buf.chunks_exact_mut(2) {
                x.swap(0, 1);
            }
        }

        Ok(())
    }
}