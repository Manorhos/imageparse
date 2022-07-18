use chd_rs::metadata::KnownMetadata::{CdRomTrack, CdRomTrack2};
use chd_rs::metadata::{ChdMetadata, ChdMetadataTag};

use text_io::try_scan;

#[derive(Debug)]
pub struct CdTrackInfo {
    pub track_no: u8,
    pub track_type: String,
    pub sub_type: String,
    pub frames: u32,

    // These are only present when using the "new" metadata format
    pub pregap: Option<u32>,
    pub pgtype: Option<String>,
    pub pgsub: Option<String>,
    pub postgap: Option<u32>,
}

impl CdTrackInfo {
    pub(super) fn from_v1_metadata(bytes: &[u8]) -> Result<CdTrackInfo, text_io::Error> {
        let track_no;
        let track_type;
        let sub_type;
        let frames;

        try_scan!(bytes.iter().copied() => "TRACK:{} TYPE:{} SUBTYPE:{} FRAMES:{}\0",
            track_no, track_type, sub_type, frames
        );

        Ok(CdTrackInfo {
            track_no,
            track_type,
            sub_type,
            frames,
            pregap: None,
            pgtype: None,
            pgsub: None,
            postgap: None,
        })
    }

    pub(super) fn from_v2_metadata(bytes: &[u8]) -> Result<CdTrackInfo, text_io::Error> {
        let track_no;
        let track_type;
        let sub_type;
        let frames;
        let pregap;
        let pgtype;
        let pgsub;
        let postgap;

        try_scan!(bytes.iter().copied() => "TRACK:{} TYPE:{} SUBTYPE:{} FRAMES:{} \
            PREGAP:{} PGTYPE:{} PGSUB:{} POSTGAP:{}\0",
            track_no, track_type, sub_type, frames,
            pregap, pgtype, pgsub, postgap
        );

        Ok(CdTrackInfo {
            track_no,
            track_type,
            sub_type,
            frames,
            pregap: Some(pregap),
            pgtype: Some(pgtype),
            pgsub: Some(pgsub),
            postgap: Some(postgap),
        })
    }
}

pub fn cd_tracks(metadata: &[ChdMetadata]) -> Result<Vec<CdTrackInfo>, text_io::Error> {
    let mut tracks = Vec::new();

    for x in metadata.iter() {
        if x.metatag() == CdRomTrack.metatag() {
            tracks.push(CdTrackInfo::from_v1_metadata(&x.value)?);
        } else if x.metatag() == CdRomTrack2.metatag() {
            tracks.push(CdTrackInfo::from_v2_metadata(&x.value)?);
        }
    }

    Ok(tracks)
}