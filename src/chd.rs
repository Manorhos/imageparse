#[cfg(feature = "multithreading")]
mod chd_thread;

mod track_metadata;

use std::collections::BTreeSet;
use std::convert::TryInto;
use std::path::Path;
use std::sync::mpsc::RecvError;

use chd_rs::Chd;
use chd_rs::metadata::Metadata;
use chd_rs::header::Header;

use log::{debug, info, trace, warn, error};

use thiserror::Error;

use crate::{Event, Image, ImageError, MsfIndex, TrackType};
use track_metadata::CdTrackInfo;

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
    ChdError(#[from] chd_rs::Error),
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error("Error while parsing track metadata: {0}")]
    TrackParseError(#[from] text_io::Error),
    #[error("CHD file does not seem like a CDROM image (wrong hunk size)")]
    WrongHunkSize,
    #[error("Wrong buffer size, needs to be 2352 bytes")]
    WrongBufferSize,
    #[error("Unsupported sector format: {0}")]
    UnsupportedSectorFormat(String),
    #[error("Error receiving hunk: {0}")]
    HunkRecvError(RecvError),
    #[error("CHD contains no CDROM tracks")]
    NoTracks,
    #[error("Recursion depth exceeded while opening parent CHDs")]
    RecursionDepthExceeded,
    #[error("Unsupported CHD format version")]
    UnsupportedChdVersion,
    #[error("Parent not found in given paths")]
    ParentNotFound,
}

pub struct ChdImage {
    #[cfg(feature = "multithreading")]
    hunk_reader: chd_thread::ChdHunkReader,

    #[cfg(not(feature = "multithreading"))]
    chd: Chd<std::fs::File>,
    tracks: Vec<Track>,

    // Intermediate buffer for the compressed data, needed for chd crate
    #[cfg(not(feature = "multithreading"))]
    comp_buf: Vec<u8>,
    hunk: Vec<u8>,
    current_hunk_no: Option<u32>,
    current_lba: u32,
    // Starts counting from 0
    current_track: usize,

    num_hunks: u32,
    hunk_len: u32,
    sectors_per_hunk: u32,

    invalid_subq_lbas: Option<BTreeSet<u32>>,
}

impl ChdImage {
    pub fn open<P>(path: P) -> Result<ChdImage, ChdImageError>
        where P: AsRef<Path>
    {
        ChdImage::_open(path.as_ref())
    }

    fn _open(path: &Path) -> Result<ChdImage, ChdImageError> {
        let chd = Chd::open(
            std::fs::File::open(path)?,
            None
        )?;
        Self::from_chd(chd, path)
    }

    /// Opens the CHD file referred to by `path` while opening parents recursively
    /// searching through the files referred to by `possible_parents`.
    ///
    /// # Note
    ///
    /// Currently only supports V3-V5 CHDs. Will skip over files that fail to open or where the
    /// version mismatches with the child CHD.
    pub fn open_with_parent<P, PP>(path: P, possible_parents: &[PP]) -> Result<ChdImage, ChdImageError>
        where P: AsRef<Path>, PP: AsRef<Path>
    {
        let possible_parents: Vec<&Path> = possible_parents.iter().map(|x| x.as_ref()).collect();
        Self::_open_with_parent(path.as_ref(), &possible_parents)
    }

    fn _open_with_parent(path: &Path, possible_parents: &[&Path]) -> Result<ChdImage, ChdImageError> {
        let file = std::fs::File::open(path)?;
        let chd = Chd::open(file, None)?;

        if !chd.header().has_parent() {
            debug!("open_with_parent: Opening CHD without a parent as it doesn't require one");
            Self::from_chd(chd, path)
        } else {
            let chd = Self::open_with_parents_recursively(path, possible_parents, 0)?;
            Self::from_chd(*chd, path)
        }
    }

    fn open_with_parents_recursively(path: &Path, possible_parents: &[&Path], depth: u8) -> Result<Box<Chd<std::fs::File>>, ChdImageError> {
        if depth >= 10 {
            return Err(ChdImageError::RecursionDepthExceeded);
        }

        let mut file = std::fs::File::open(path)?;
        let child_header = Header::try_read_header(&mut file)?;

        if !child_header.has_parent() {
            Ok(Box::new(Chd::open(
                file,
                None
            )?))
        } else {
            let parent_sha1 = if let Some(sha1) = child_header.parent_sha1() {
                sha1
            } else {
                return Err(ChdImageError::UnsupportedChdVersion)
            };

            for p in possible_parents {
                let sha1 = match Self::chd_header_sha1(p.as_ref()) {
                    Ok(Some(sha1)) => sha1,
                    Ok(None) => {
                        warn!("Skipped possible parent CHD {:?} because \
                            no SHA-1 is present in the header", p);
                        continue;
                    }
                    Err(e) => {
                        warn!("Skipped possible parent CHD {:?} due \
                            to error: {:?}", p, e);
                        continue;
                    }
                };

                if sha1 == parent_sha1 {
                    debug!("Opening child {:?} with parent {:?}", path, p);
                    let parent = Self::open_with_parents_recursively(p.as_ref(), possible_parents, depth + 1)?;
                    return Ok(Box::new(Chd::open(
                        file,
                        Some(parent)
                    )?))
                }
            }

            Err(ChdImageError::ParentNotFound)
        }
    }

    fn chd_header_sha1(path: &Path) -> Result<Option<[u8;20]>, ChdImageError> {
        let mut parent_file = std::fs::File::open(path)?;
        let chd_header = Header::try_read_header(&mut parent_file)?;
        Ok(chd_header.sha1())
    }

    fn from_chd(mut chd: Chd<std::fs::File>, path: &Path) -> Result<ChdImage, ChdImageError> {
        let num_hunks = chd.header().hunk_count();
        let hunk_len = chd.header().hunk_size();
        let sectors_per_hunk = hunk_len / BYTES_PER_SECTOR;

        if hunk_len % BYTES_PER_SECTOR != 0 {
            return Err(ChdImageError::WrongHunkSize);
        }

        let mut hunk = chd.get_hunksized_buffer();
        let mut comp_buf = Vec::new();
        chd.hunk(0)?.read_hunk_in(&mut comp_buf, &mut hunk)?;

        let mut tracks = Vec::new();

        let metadata: Vec<Metadata> = chd.metadata_refs().try_into()?;
        let chd_tracks = track_metadata::cd_tracks(&metadata[..])?;
        if chd_tracks.is_empty() {
            return Err(ChdImageError::NoTracks);
        }

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

        Ok(ChdImage {
            #[cfg(feature = "multithreading")]
            hunk_reader: chd_thread::ChdHunkReader::new(chd),
            #[cfg(not(feature = "multithreading"))]
            chd,

            #[cfg(not(feature = "multithreading"))]
            comp_buf,
            hunk,
            current_hunk_no: Some(0),
            current_lba: 150,
            current_track: 0,

            num_hunks,
            hunk_len,
            sectors_per_hunk,

            tracks,

            invalid_subq_lbas,
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

    #[cfg(not(feature = "multithreading"))]
    fn read_hunk(&mut self, hunk_no: u32) -> Result<usize, chd_rs::Error> {
        self.chd.hunk(hunk_no)?.read_hunk_in(&mut self.comp_buf, &mut self.hunk)
    }

    #[cfg(feature = "multithreading")]
    fn read_hunk(&mut self, hunk_no: u32) -> Result<(), ChdImageError> {
        // Clear completion
        if self.hunk_reader.hunk_read_pending() {
            let time = std::time::Instant::now();
            let _ = self.hunk_reader.recv_completion();
            debug!("Wasted {:?} waiting for old completion", time.elapsed());
        }

        if let Some(hunk) = self.hunk_reader.get_hunk_from_cache(hunk_no) {
            self.hunk = hunk;
            debug!("Got new hunk from cache");
            // Send prefetch to notify thread of us reading the hunk so it
            // can prefetch more
            self.hunk_reader.send_prefetch_hunk_command(hunk_no);
        } else {
            self.hunk_reader.send_read_hunk_command(hunk_no);
        }
        Ok(())
    }

    fn hunk_no_for_lba(&self, lba: u32) -> Result<u32, ImageError> {
        let current_track = &self.tracks[self.current_track];

        if lba < FIRST_TRACK_PREGAP {
            return Err(ImageError::OutOfRange);
        }

        let lba = lba + current_track.padding_offset - FIRST_TRACK_PREGAP;
        let hunk_no = lba / self.sectors_per_hunk;
        trace!("hunk_no_for_lba {} -> {}", lba, hunk_no);
        if hunk_no > self.num_hunks {
            Err(ImageError::OutOfRange)
        } else {
            Ok(hunk_no)
        }
    }

    fn set_location_lba(&mut self, lba: u32) -> Result<(), ImageError> {
        self.current_lba = lba;
        // Set this to None so any upcoming errors making us return early don't
        // leave us in an inconsistent state
        let current_hunk_no = self.current_hunk_no;
        self.current_hunk_no = None;

        // TODO: Can we really assume that the first track's pregap is always
        // two seconds long?
        if lba < FIRST_TRACK_PREGAP {
            self.current_track = 0;
            return Ok(());
        }

        self.update_current_track(lba)?;

        let hunk_no = self.hunk_no_for_lba(lba)?;
        debug!("set_location_lba {} -> hunk_no {}", lba, hunk_no);
        if hunk_no != current_hunk_no.unwrap_or(u32::MAX) {
            if let Err(e) = self.read_hunk(hunk_no) {
                debug!("set_location_lba: {:?}", e);
                return Err(ChdImageError::from(e).into());
            }
        }
        self.current_hunk_no = Some(hunk_no);
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
            let len = self.num_hunks * self.hunk_len;
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

    #[cfg(feature = "multithreading")]
    fn advise_prefetch(&mut self, location: MsfIndex) {
        let hunk_no = self.hunk_no_for_lba(location.to_lba());
        if let Ok(hunk_no) = hunk_no {
            self.hunk_reader.send_prefetch_hunk_command(hunk_no);
        }
    }

    fn copy_current_sector(&mut self, buf: &mut[u8]) -> Result<(), ImageError> {
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

        if self.current_hunk_no.is_none() {
            warn!("Last read of this hunk failed, retrying");
            self.set_location_lba(self.current_lba)?;
        }
        assert_eq!(self.current_hunk_no, Some(self.hunk_no_for_lba(self.current_lba)?));

        #[cfg(feature = "multithreading")]
        if self.hunk_reader.hunk_read_pending() {
            let now = std::time::Instant::now();
            let recv = self.hunk_reader.recv_completion();
            if let Ok(completion) = recv {
                if let Ok(hunk_no) = completion {
                    assert_eq!(self.current_hunk_no, Some(hunk_no));
                    self.hunk = self.hunk_reader.get_hunk_from_cache(hunk_no)
                                    .expect("BUG: Hunk not in cache even though it should be");
                    debug!("Receiving hunk took {:?}", now.elapsed());
                } else {
                    self.current_hunk_no = None;
                    return Err(ChdImageError::ChdError(completion.unwrap_err()).into());
                }
            } else {
                self.current_hunk_no = None;
                return Err(ChdImageError::HunkRecvError(recv.unwrap_err()).into());
            }
        }

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