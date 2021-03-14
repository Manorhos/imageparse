use crate::{Event, Image, ImageError, MsfIndex, TrackType};

use std::path::Path;

use chdr::{ChdError, ChdFile};

use thiserror::Error;


const BYTES_PER_SECTOR: u32 =  2352 + 96;

#[derive(Debug, Error)]
pub enum ChdImageError {
    #[error(transparent)]
    ChdError(#[from] ChdError),
    #[error("CHD file does not seem like a CDROM image (wrong hunk size)")]
    WrongHunkSize,
    #[error("Wrong buffer size, needs to be 2352 bytes")]
    WrongBufferSize,
}
pub struct ChdImage {
    chd: ChdFile,
    hunk: Vec<u8>,
    current_hunk_no: u32,
    current_lba: u32,

    sectors_per_hunk: u32,
}

impl ChdImage {
    pub fn open<P>(path: P) -> Result<ChdImage, ChdImageError>
        where P: AsRef<Path>
    {
        let mut chd = ChdFile::open(path)?;

        if chd.hunk_len() % BYTES_PER_SECTOR != 0 {
            return Err(ChdImageError::WrongHunkSize);
        }

        let mut hunk = vec![0; chd.hunk_len() as usize];
        chd.read_hunk(0, &mut hunk[..])?;

        let sectors_per_hunk = chd.hunk_len() / BYTES_PER_SECTOR;

        Ok(ChdImage {
            chd,
            hunk,
            current_hunk_no: 0,
            current_lba: 150,

            sectors_per_hunk,
        })
    }

    fn set_location_lba(&mut self, lba: u32) -> Result<(), ImageError> {
        self.current_lba = lba;
        // TODO: Can we really assume that the first track's pregap is always
        // two seconds long?
        if lba < 150 {
            return Ok(());
        }
        let lba = lba - 150;
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
        // TODO
        1
    }

    fn current_subchannel_q_valid(&self) -> bool {
        // TODO
        true
    }

    fn current_track(&self) -> Result<u8, ImageError> {
        // TODO
        Ok(1)
    }

    fn current_index(&self) -> Result<u8, ImageError> {
        // TODO
        Ok(1)
    }

    fn current_track_local_msf(&self) -> Result<MsfIndex, ImageError> {
        // TODO
        Ok(MsfIndex::from_lba(self.current_lba)?)
    }

    fn current_global_msf(&self) -> Result<MsfIndex, ImageError> {
        Ok(MsfIndex::from_lba(self.current_lba)?)
    }

    fn current_track_type(&self) -> Result<TrackType, ImageError> {
        // TODO
        Ok(TrackType::Mode1)
    }

    fn first_track_type(&self) -> TrackType {
        // TODO
        TrackType::Mode1
    }

    fn track_start(&self, track: u8) -> Result<MsfIndex, ImageError> {
        // Track 0: Special case for PlayStation, return length of whole disc
        // TODO: Make this less ugly?
        if track == 0 {
            let len = self.chd.num_hunks() * self.chd.hunk_len();
            let num_sectors = 150 + len / BYTES_PER_SECTOR;
            Ok(MsfIndex::from_lba(num_sectors)?)
        } else if track == 1 {
            // TODO?
            Ok(MsfIndex::from_lba(150)?)
        } else {
            // TODO
            Err(ImageError::OutOfRange)
        }
    }

    fn set_location(&mut self, target: MsfIndex) -> Result<(), ImageError> {
        self.set_location_lba(target.to_lba())
    }

    fn set_location_to_track(&mut self, track: u8) -> Result<(), ImageError> {
        if track != 1 {
            Err(ImageError::OutOfRange)
        } else {
            self.set_location_lba(150)
        }
    }

    fn advance_position(&mut self) -> Result<Option<Event>, ImageError> {
        let res = self.set_location_lba(self.current_lba + 1);
        if let Err(e) = res {
            if let ImageError::OutOfRange = e {
                Ok(Some(Event::EndOfDisc))
            } else {
                Err(e)
            }
        } else {
            Ok(None)
        }
    }

    fn copy_current_sector(&self, buf: &mut[u8]) -> Result<(), ImageError> {
        if buf.len() != 2352 {
            return Err(ChdImageError::WrongBufferSize.into())
        }
        // TODO: Can we really assume that the first track's pregap is always
        // two seconds long?
        if self.current_lba < 150 {
            buf.fill(0);
            return Ok(());
        }
        let sector_in_hunk = (self.current_lba - 150) % self.sectors_per_hunk;
        let sector_start = (sector_in_hunk * BYTES_PER_SECTOR) as usize;
        let sector = &self.hunk[sector_start..sector_start + 2352];
        buf.clone_from_slice(sector);
        Ok(())
    }
}