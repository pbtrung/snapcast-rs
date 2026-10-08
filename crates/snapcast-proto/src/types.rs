//! Shared types used across the protocol.

use std::io::{self, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

/// Timestamp with second and microsecond components.
///
/// Matches the C++ `tv` struct used throughout the Snapcast protocol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timeval {
    /// Seconds component.
    pub sec: i32,
    /// Microseconds component.
    pub usec: i32,
}

impl Timeval {
    /// Create from microseconds since epoch.
    pub fn from_usec(usec: i64) -> Self {
        Self {
            sec: (usec / 1_000_000) as i32,
            usec: (usec % 1_000_000) as i32,
        }
    }

    /// Convert to microseconds since epoch.
    pub fn to_usec(self) -> i64 {
        self.sec as i64 * 1_000_000 + self.usec as i64
    }

    /// Read a Timeval (8 bytes, little-endian) from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            sec: r.read_i32::<LittleEndian>()?,
            usec: r.read_i32::<LittleEndian>()?,
        })
    }

    /// Write a Timeval (8 bytes, little-endian) to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_i32::<LittleEndian>(self.sec)?;
        w.write_i32::<LittleEndian>(self.usec)?;
        Ok(())
    }
}

// The operands may come straight off the wire (`BaseMessage::sent` is set by
// the peer), so the arithmetic must not panic on overflow. It is done in i64,
// the microseconds are normalized into `0..1_000_000`, and the seconds wrap
// on overflow (as release-mode `i32` arithmetic, and the C++ `tv`, would).
impl Timeval {
    fn normalized(sec: i64, usec: i64) -> Self {
        Self {
            sec: (sec + usec.div_euclid(1_000_000)) as i32,
            usec: usec.rem_euclid(1_000_000) as i32,
        }
    }
}

impl std::ops::Add for Timeval {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self::normalized(
            i64::from(self.sec) + i64::from(rhs.sec),
            i64::from(self.usec) + i64::from(rhs.usec),
        )
    }
}

impl std::ops::Sub for Timeval {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self::normalized(
            i64::from(self.sec) - i64::from(rhs.sec),
            i64::from(self.usec) - i64::from(rhs.usec),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeval_add() {
        let a = Timeval {
            sec: 1,
            usec: 900_000,
        };
        let b = Timeval {
            sec: 0,
            usec: 200_000,
        };
        let result = a + b;
        assert_eq!(
            result,
            Timeval {
                sec: 2,
                usec: 100_000
            }
        );
    }

    #[test]
    fn timeval_sub() {
        let a = Timeval {
            sec: 2,
            usec: 100_000,
        };
        let b = Timeval {
            sec: 1,
            usec: 900_000,
        };
        let result = a - b;
        assert_eq!(
            result,
            Timeval {
                sec: 0,
                usec: 200_000
            }
        );
    }

    #[test]
    fn timeval_arithmetic_on_extreme_wire_values_does_not_panic() {
        // Regression: `received - sent` with a peer-supplied `sent` used to
        // overflow `i32` (a panic in debug builds).
        let min = Timeval {
            sec: i32::MIN,
            usec: i32::MIN,
        };
        let max = Timeval {
            sec: i32::MAX,
            usec: i32::MAX,
        };
        for (a, b) in [(max, min), (min, max), (max, max), (min, min)] {
            for r in [a - b, a + b] {
                assert!((0..1_000_000).contains(&r.usec), "{r:?}");
            }
        }
        // In-range results stay exact.
        let a = Timeval { sec: 5, usec: 0 };
        let b = Timeval {
            sec: 0,
            usec: -2_500_000,
        };
        assert_eq!((a + b).to_usec(), 2_500_000);
        assert_eq!((a - b).to_usec(), 7_500_000);
    }

    #[test]
    fn timeval_round_trip() {
        let tv = Timeval {
            sec: 1000,
            usec: 500_000,
        };
        let mut buf = Vec::new();
        tv.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), 8);
        let mut cursor = io::Cursor::new(&buf);
        let decoded = Timeval::read_from(&mut cursor).unwrap();
        assert_eq!(tv, decoded);
    }

    #[test]
    fn timeval_known_bytes() {
        // sec=1000 (0x000003E8), usec=500000 (0x0007A120), little-endian
        let expected: [u8; 8] = [0xE8, 0x03, 0x00, 0x00, 0x20, 0xA1, 0x07, 0x00];
        let tv = Timeval {
            sec: 1000,
            usec: 500_000,
        };
        let mut buf = Vec::new();
        tv.write_to(&mut buf).unwrap();
        assert_eq!(buf.as_slice(), &expected);
    }
}
