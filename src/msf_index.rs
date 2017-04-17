use std;
use std::cmp::Ordering;
use std::error::Error;
use std::fmt;
use std::ops::{Add, Sub};


#[derive(Debug, PartialEq)]
pub enum MsfParseError {
    ParseIntError(std::num::ParseIntError),
    OutOfRangeError,
    InvalidMsfError
}

impl Error for MsfParseError {
    fn description(&self) -> &str {
        "Could not parse MSF Timestamp"
    }

    fn cause(&self) -> Option<&Error> {
        use MsfParseError::*;
        match *self {
            ParseIntError(ref inner_err) => Some(inner_err),
            _ => None
        }
    }
}

impl fmt::Display for MsfParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.description())
    }
}

impl From<std::num::ParseIntError> for MsfParseError {
    fn from(err: std::num::ParseIntError) -> MsfParseError {
        MsfParseError::ParseIntError(err)
    }
}


// MsfIndex(minutes, seconds, frame), not BCD encoded
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

    pub fn from_sectors(mut sectors: usize) -> Result<MsfIndex, MsfParseError> {
        let m = sectors / (60 * 75);
        sectors -= m * 60 * 75;
        let s = sectors / 60;
        sectors -= s * 60;
        let f = sectors;
        MsfIndex::new(m as u8, s as u8, f as u8)
    }

    pub fn to_offset(&self) -> usize {
        self.to_sectors() * 2352
    }

    pub fn to_sectors(&self) -> usize {
        let MsfIndex(m,s,f) = *self;
        (m as usize * 60 * 75 + s as usize * 75 + f as usize)
    }

    pub fn next(&self) -> Result<MsfIndex, MsfOverflow> {
        *self + MsfIndex::new(0, 0, 1).unwrap()
    }

    pub fn to_bcd_values(&self) -> (u8, u8, u8) {
        let MsfIndex(m,s,f) = *self;
        let m_bcd = ((m / 10) << 4) + (m % 10);
        let s_bcd = ((s / 10) << 4) + (s % 10);
        let f_bcd = ((f / 10) << 4) + (f % 10);
        debug!("Converted from ({}, {}, {}) to (0x{:x}, 0x{:x}, 0x{:x}",
               m, s, f, m_bcd, s_bcd, f_bcd);
        (m_bcd, s_bcd, f_bcd)
    }
}

impl fmt::Display for MsfIndex {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({}, {}, {})", self.0, self.1, self.2)
    }
}


#[derive(Debug)]
pub enum MsfOperation {
    Add,
    Sub
}

impl fmt::Display for MsfOperation {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::MsfOperation::*;
        match *self {
            Add => write!(f, "+"),
            Sub => write!(f, "-")
        }
    }
}


#[derive(Debug)]
pub struct MsfOverflow(MsfOperation, MsfIndex, MsfIndex);

impl Error for MsfOverflow {
    fn description(&self) -> &str {
        "Addition of two MsfIndex values overflowed"
    }

    fn cause(&self) -> Option<&Error> {
        None
    }
}

impl fmt::Display for MsfOverflow {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "The following operation on these two MsfIndex values overflowed: {} {} {}",
               self.1, self.0, self.2)
    }
}


impl Add for MsfIndex {
    type Output = Result<MsfIndex, MsfOverflow>;

    fn add(self, other: MsfIndex) -> Result<MsfIndex, MsfOverflow> {
        let mut new_msf_tuple = (self.0 + other.0,
                                 self.1 + other.1,
                                 self.2 + other.2);

        // 75 frames -> 1 second
        while new_msf_tuple.2 >= 75 {
            new_msf_tuple.2 -= 75;
            new_msf_tuple.1 += 1;
        }

        // 60 seconds -> 1 minute
        while new_msf_tuple.1 >= 60 {
            new_msf_tuple.1 -= 60;
            new_msf_tuple.0 += 1;
        }

        match MsfIndex::new(new_msf_tuple.0, new_msf_tuple.1, new_msf_tuple.2)
        {
            Ok(result) => Ok(result),
            Err(_) => Err(MsfOverflow(MsfOperation::Add, self, other))
        }
    }
}

impl Sub for MsfIndex {
    type Output = Result<MsfIndex, MsfOverflow>;

    fn sub(self, other: MsfIndex) -> Result<MsfIndex, MsfOverflow> {
        if other > self {
            return Err(MsfOverflow(MsfOperation::Sub, self, other));
        }

        let mut tmp: (i16, i16, i16) = (
            self.0 as i16 - other.0 as i16,
            self.1 as i16 - other.1 as i16,
            self.2 as i16 - other.2 as i16);
        
        if tmp.2 < 0 {
            tmp.1 -= 1;
            tmp.2 += 75;
        }

        if tmp.1 < 0 {
            tmp.0 -= 1;
            tmp.1 += 60;
        }

        assert!(tmp.0 >= 0, "{} - {}", self, other);

        match MsfIndex::new(tmp.0 as u8, tmp.1 as u8, tmp.2 as u8)
        {
            Ok(result) => Ok(result),
            Err(_) => Err(MsfOverflow(MsfOperation::Sub, self, other))
        }
    }
}

impl Ord for MsfIndex {
    fn cmp(&self, other: &MsfIndex) -> Ordering {
        let self_sector = self.to_sectors();
        let other_sector = &other.to_sectors();
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
    use msf_index::*;

    #[test]
    fn msf_index_range() {
        assert_eq!(MsfIndex::new(0, 0, 0).unwrap(), MsfIndex(0, 0, 0));
        assert_eq!(MsfIndex::new(13, 37, 42).unwrap(), MsfIndex(13, 37, 42));
        assert_eq!(MsfIndex::new(99, 59, 74).unwrap(), MsfIndex(99, 59, 74));

        assert_eq!(MsfIndex::new(99, 59, 75), Err(MsfParseError::OutOfRangeError));
        assert_eq!(MsfIndex::new(99, 60, 74), Err(MsfParseError::OutOfRangeError));
        assert_eq!(MsfIndex::new(100, 59, 74), Err(MsfParseError::OutOfRangeError));
    }

    #[test]
    fn msf_add() {
        let msf_0 = MsfIndex::new(0, 0, 0).unwrap();
        let msf_0_0_1 = MsfIndex::new(0, 0, 1).unwrap();
        let msf_13_37_42 = MsfIndex::new(13, 37, 42).unwrap();
        let msf_max_f = MsfIndex::new(0, 0, 74).unwrap();
        let msf_max_sf = MsfIndex::new(0, 59, 74).unwrap();
        let msf_max_msf = MsfIndex::new(99, 59, 74).unwrap();

        assert_eq!((msf_0 + msf_0_0_1).unwrap(), msf_0_0_1);
        assert_eq!((msf_13_37_42 + msf_0_0_1).unwrap(), MsfIndex(13, 37, 43));
        assert_eq!((msf_max_f + msf_0_0_1).unwrap(), MsfIndex(0, 1, 0));
        assert_eq!((msf_max_sf + msf_0_0_1).unwrap(), MsfIndex(1, 0, 0));
        assert!((msf_max_msf + msf_0_0_1).is_err());
    }
}
