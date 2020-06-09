use std;
use std::cmp::Ordering;
use std::error::Error;
use std::fmt;


#[derive(Debug, PartialEq)]
pub enum MsfParseError {
    ParseIntError(std::num::ParseIntError),
    OutOfRangeError,
    InvalidMsfError
}

impl Error for MsfParseError {
    fn cause(&self) -> Option<&dyn Error> {
        use MsfParseError::*;
        match *self {
            ParseIntError(ref inner_err) => Some(inner_err),
            _ => None
        }
    }
}

impl fmt::Display for MsfParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Could not parse MSF Timestamp")
    }
}

impl From<std::num::ParseIntError> for MsfParseError {
    fn from(err: std::num::ParseIntError) -> MsfParseError {
        MsfParseError::ParseIntError(err)
    }
}


// MsfIndex(minutes, seconds, frame), not BCD encoded
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde-support", derive(Serialize, Deserialize))]
pub struct MsfIndex(u8, u8, u8);

impl MsfIndex {
    pub fn new(m: u8, s: u8, f: u8) -> Result<MsfIndex, MsfParseError> {
        if m > 99 || s > 59 || f > 74 {
            Err(MsfParseError::OutOfRangeError)
        } else {
            Ok(MsfIndex(m,s,f))
        }
    }

    pub fn from_bcd_values(m_bcd: u8, s_bcd: u8, f_bcd: u8) -> Result<MsfIndex, MsfParseError> {
        if (m_bcd & 0xf0) > 0x90 || (m_bcd & 0x0f) > 0x09 ||
           (s_bcd & 0xf0) > 0x90 || (s_bcd & 0x0f) > 0x09 ||
           (f_bcd & 0xf0) > 0x90 || (f_bcd & 0x0f) > 0x09 {
            Err(MsfParseError::OutOfRangeError)
        } else {
            let m = (m_bcd >> 4) * 10 + (m_bcd & 0x0f);
            let s = (s_bcd >> 4) * 10 + (s_bcd & 0x0f);
            let f = (f_bcd >> 4) * 10 + (f_bcd & 0x0f);
            debug!("from_bcd_values: Converted (0x{:x}, 0x{:x}, 0x{:x}) to ({}, {}, {})",
                   m_bcd, s_bcd, f_bcd, m, s, f);
            MsfIndex::new(m,s,f)
        }
    }

    pub fn try_from_str(s: &str) -> Result<MsfIndex, MsfParseError> {
        let s = s.trim();
        let colon_matches = s.split(":").collect::<Vec<&str>>();
        debug!("{:?}", colon_matches);
        if colon_matches.len() == 3 {
            let (m, s, f) = (
                colon_matches[0].parse()?,
                colon_matches[1].parse()?,
                colon_matches[2].parse()?
            );
            MsfIndex::new(m, s, f)
        } else {
            Err(MsfParseError::InvalidMsfError)
        }
    }

    pub fn from_lba(sector_no: u32) -> Result<MsfIndex, MsfParseError> {
        let mut temp_sectors = sector_no;
        let m = temp_sectors / (60 * 75);
        temp_sectors -= m * 60 * 75;
        let s = temp_sectors / 75;
        temp_sectors -= s * 75;
        let f = temp_sectors;
        debug!("{} -> ({},{},{})", sector_no, m, s, f);
        MsfIndex::new(m as u8, s as u8, f as u8)
    }

    pub fn to_lba(&self) -> u32 {
        let MsfIndex(m,s,f) = *self;
        m as u32 * 60 * 75 + s as u32 * 75 + f as u32
    }

    pub fn to_bcd_values(&self) -> (u8, u8, u8) {
        let MsfIndex(m,s,f) = *self;
        let m_bcd = ((m / 10) << 4) + (m % 10);
        let s_bcd = ((s / 10) << 4) + (s % 10);
        let f_bcd = ((f / 10) << 4) + (f % 10);
        debug!("Converted from ({}, {}, {}) to (0x{:x}, 0x{:x}, 0x{:x})",
               m, s, f, m_bcd, s_bcd, f_bcd);
        (m_bcd, s_bcd, f_bcd)
    }
}

impl fmt::Display for MsfIndex {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({}, {}, {})", self.0, self.1, self.2)
    }
}


impl Ord for MsfIndex {
    fn cmp(&self, other: &MsfIndex) -> Ordering {
        let self_sector = self.to_lba();
        let other_sector = &other.to_lba();
        self_sector.cmp(other_sector)
    }
}

impl PartialOrd for MsfIndex {
    fn partial_cmp(&self, other: &MsfIndex) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use index::*;

    #[test]
    fn msf_index_range() {
        assert_eq!(MsfIndex::new(0, 0, 0).unwrap(), MsfIndex(0, 0, 0));
        assert_eq!(MsfIndex::new(13, 37, 42).unwrap(), MsfIndex(13, 37, 42));
        assert_eq!(MsfIndex::new(99, 59, 74).unwrap(), MsfIndex(99, 59, 74));

        assert_eq!(MsfIndex::new(99, 59, 75), Err(MsfParseError::OutOfRangeError));
        assert_eq!(MsfIndex::new(99, 60, 74), Err(MsfParseError::OutOfRangeError));
        assert_eq!(MsfIndex::new(100, 59, 74), Err(MsfParseError::OutOfRangeError));
    }
}
