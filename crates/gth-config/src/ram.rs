//! RAM variables: where a value (score, lives, ...) lives in a core's memory and how it is
//! encoded (docs/games.md, "RAM variables").

use gth_libretro_sys::{
    RETRO_MEMORY_RTC, RETRO_MEMORY_SAVE_RAM, RETRO_MEMORY_SYSTEM_RAM, RETRO_MEMORY_VIDEO_RAM,
};
use serde::{Deserialize, Serialize};

/// A core memory exposed through `retro_get_memory_data`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryId {
    SystemRam,
    SaveRam,
    VideoRam,
    Rtc,
}

impl MemoryId {
    pub const ALL: [MemoryId; 4] = [
        MemoryId::SystemRam,
        MemoryId::SaveRam,
        MemoryId::VideoRam,
        MemoryId::Rtc,
    ];

    /// Its name in `game.toml` (`system_ram`, ...).
    pub fn name(self) -> &'static str {
        match self {
            MemoryId::SystemRam => "system_ram",
            MemoryId::SaveRam => "save_ram",
            MemoryId::VideoRam => "video_ram",
            MemoryId::Rtc => "rtc",
        }
    }

    /// The `RETRO_MEMORY_*` id.
    pub fn id(self) -> u32 {
        match self {
            MemoryId::SystemRam => RETRO_MEMORY_SYSTEM_RAM,
            MemoryId::SaveRam => RETRO_MEMORY_SAVE_RAM,
            MemoryId::VideoRam => RETRO_MEMORY_VIDEO_RAM,
            MemoryId::Rtc => RETRO_MEMORY_RTC,
        }
    }
}

/// Where a variable's bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// `offset` bytes into a core memory.
    Memory { memory: MemoryId, offset: usize },
    /// An emulated address in the core's memory map (`SET_MEMORY_MAPS`).
    Address(usize),
}

/// How the bytes, after the optional 16-bit swap, become a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    /// Packed BCD, two decimal digits per byte, most significant byte first.
    Bcd,
    /// Unsigned little-endian.
    Le,
    /// Unsigned big-endian.
    Be,
    /// Signed (two's complement) little-endian: a score that goes negative (the ALE core's,
    /// docs/cores.md).
    Sle,
    /// One decimal digit (0 to 9) per byte, most significant first: a score a game keeps as
    /// the digits it draws.
    Digits,
}

impl Encoding {
    /// The encodings a RAM search reads candidates in (its views), not all of them: `Digits`
    /// is left out, since a search can't find a variable by one-digit-per-byte, so its bytes
    /// are found by looking at the RAM (docs/games.md, "RAM variables"), and so is `Sle`, whose
    /// non-negative values the `le` views already find.
    pub const SEARCHABLE: [Encoding; 3] = [Encoding::Bcd, Encoding::Le, Encoding::Be];

    /// Its name in `game.toml` (`bcd`, `le`, `be`, `sle`, `digits`).
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Bcd => "bcd",
            Encoding::Le => "le",
            Encoding::Be => "be",
            Encoding::Sle => "sle",
            Encoding::Digits => "digits",
        }
    }
}

/// One named RAM variable. Built only through validation, so its fields always agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawRamVar", into = "RawRamVar")]
pub struct RamVar {
    pub location: Location,
    /// Bytes, 1 to 8 (even when `swap16`).
    pub size: usize,
    pub encoding: Encoding,
    /// Swap the two bytes of each 16-bit word before decoding (FBNeo keeps 68k RAM as
    /// host-endian 16-bit words).
    pub swap16: bool,
    /// The decoded value is multiplied by this (a score stored in tens has `scale = 10`).
    pub scale: u32,
}

/// The TOML form: `memory` + `offset`, or `address`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRamVar {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    memory: Option<MemoryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offset: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    address: Option<usize>,
    size: usize,
    encoding: Encoding,
    #[serde(default)]
    swap16: bool,
    #[serde(default = "one")]
    scale: u32,
}

fn one() -> u32 {
    1
}

impl TryFrom<RawRamVar> for RamVar {
    type Error = String;

    fn try_from(raw: RawRamVar) -> Result<Self, String> {
        let location = match (raw.memory, raw.offset, raw.address) {
            (Some(memory), Some(offset), None) => Location::Memory { memory, offset },
            (None, None, Some(address)) => Location::Address(address),
            _ => return Err("give either `memory` and `offset`, or `address`".into()),
        };
        if !(1..=8).contains(&raw.size) {
            return Err(format!("size must be 1 to 8 bytes, not {}", raw.size));
        }
        if raw.swap16 && !raw.size.is_multiple_of(2) {
            return Err(format!("swap16 needs an even size, not {}", raw.size));
        }
        if raw.scale == 0 {
            return Err("scale must be at least 1".into());
        }
        let var = RamVar {
            location,
            size: raw.size,
            encoding: raw.encoding,
            swap16: raw.swap16,
            scale: raw.scale,
        };
        // A signed variable's largest magnitude is its most negative value, which may reach
        // i64::MIN (one past i64::MAX).
        let limit = i64::MAX as u128 + u128::from(var.encoding == Encoding::Sle);
        let max = var.max_raw() * u128::from(var.scale);
        if max > limit {
            return Err(format!(
                "{} {:?} bytes x scale {} can reach {max}, which doesn't fit an i64",
                var.size, var.encoding, var.scale
            ));
        }
        Ok(var)
    }
}

impl From<RamVar> for RawRamVar {
    fn from(var: RamVar) -> Self {
        let (memory, offset, address) = match var.location {
            Location::Memory { memory, offset } => (Some(memory), Some(offset), None),
            Location::Address(address) => (None, None, Some(address)),
        };
        RawRamVar {
            memory,
            offset,
            address,
            size: var.size,
            encoding: var.encoding,
            swap16: var.swap16,
            scale: var.scale,
        }
    }
}

impl RamVar {
    /// The largest magnitude `size` bytes can hold in this encoding, before scaling (for
    /// `Sle`, that of its most negative value).
    fn max_raw(&self) -> u128 {
        let digits = 2 * self.size as u32;
        match self.encoding {
            Encoding::Bcd => 10u128.pow(digits) - 1,
            Encoding::Le | Encoding::Be => (1u128 << (4 * digits)) - 1,
            Encoding::Sle => 1u128 << (4 * digits - 1),
            Encoding::Digits => 10u128.pow(self.size as u32) - 1,
        }
    }

    /// Decodes the variable's `size` bytes. Fails on a BCD nibble or a `digits` byte above 9
    /// (or, for a variable
    /// built without validation, a value that doesn't fit an `i64` after scaling: validation
    /// rejects any variable whose largest value wouldn't).
    pub fn decode(&self, input: &[u8]) -> Result<i64, String> {
        assert_eq!(input.len(), self.size, "caller reads `size` bytes");
        let mut buffer = [0u8; 8];
        let bytes = &mut buffer[..self.size];
        bytes.copy_from_slice(input);
        if self.swap16 {
            bytes
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .for_each(|word| word.swap(0, 1));
        }
        let le = |bytes: &[u8]| bytes.iter().rev().fold(0u64, |v, &b| v << 8 | u64::from(b));
        if self.encoding == Encoding::Sle {
            // Sign-extend from the top bit of the `size` bytes.
            let shift = 64 - 8 * self.size as u32;
            let value = ((le(bytes) << shift) as i64) >> shift;
            return value
                .checked_mul(i64::from(self.scale))
                .ok_or_else(|| format!("{value} x {} doesn't fit an i64", self.scale));
        }
        let value = match self.encoding {
            Encoding::Le => le(bytes),
            Encoding::Sle => unreachable!("decoded above"),
            Encoding::Be => bytes.iter().fold(0u64, |v, &b| v << 8 | u64::from(b)),
            Encoding::Bcd => {
                let mut value = 0u64;
                for &byte in bytes.iter() {
                    let (high, low) = (byte >> 4, byte & 0xF);
                    if high > 9 || low > 9 {
                        return Err(format!("byte {byte:#04x} isn't packed BCD"));
                    }
                    value = value * 100 + u64::from(high) * 10 + u64::from(low);
                }
                value
            }
            Encoding::Digits => {
                let mut value = 0u64;
                for &byte in bytes.iter() {
                    if byte > 9 {
                        return Err(format!("byte {byte:#04x} isn't a decimal digit"));
                    }
                    value = value * 10 + u64::from(byte);
                }
                value
            }
        };
        value
            .checked_mul(u64::from(self.scale))
            .and_then(|v| i64::try_from(v).ok())
            .ok_or_else(|| format!("{value} x {} doesn't fit an i64", self.scale))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(size: usize, encoding: Encoding, swap16: bool, scale: u32) -> RamVar {
        RamVar {
            location: Location::Address(0),
            size,
            encoding,
            swap16,
            scale,
        }
    }

    #[test]
    fn decodes_each_encoding() {
        let bytes = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(
            var(4, Encoding::Bcd, false, 1).decode(&bytes),
            Ok(12_345_678)
        );
        assert_eq!(
            var(4, Encoding::Be, false, 1).decode(&bytes),
            Ok(0x1234_5678)
        );
        assert_eq!(
            var(4, Encoding::Le, false, 1).decode(&bytes),
            Ok(0x7856_3412)
        );
        assert_eq!(
            var(4, Encoding::Bcd, true, 10).decode(&bytes),
            Ok(341_278_560)
        );
        assert_eq!(
            var(2, Encoding::Le, true, 1).decode(&bytes[..2]),
            Ok(0x1234)
        );
        let max = var(8, Encoding::Bcd, false, 1).decode(&[0x99; 8]);
        assert_eq!(max, Ok(9_999_999_999_999_999));
        // One digit per byte, most significant first; swap16 and scale apply as for the rest.
        let digits = [0, 4, 7, 6, 1, 0];
        assert_eq!(
            var(6, Encoding::Digits, false, 1).decode(&digits),
            Ok(47_610)
        );
        assert_eq!(
            var(6, Encoding::Digits, false, 10).decode(&digits),
            Ok(476_100)
        );
        assert_eq!(var(2, Encoding::Digits, true, 1).decode(&[1, 2]), Ok(21));
    }

    #[test]
    fn decodes_signed_little_endian() {
        // The ALE core's score: an i32, -21 for a lost game of Pong.
        let pong = (-21i32).to_le_bytes();
        assert_eq!(var(4, Encoding::Sle, false, 1).decode(&pong), Ok(-21));
        assert_eq!(var(4, Encoding::Sle, false, 10).decode(&pong), Ok(-210));
        assert_eq!(
            var(4, Encoding::Sle, false, 1).decode(&[21, 0, 0, 0]),
            Ok(21)
        );
        // The sign is the top bit of `size` bytes, whatever the size.
        assert_eq!(var(1, Encoding::Sle, false, 1).decode(&[0xFF]), Ok(-1));
        assert_eq!(var(1, Encoding::Sle, false, 1).decode(&[0x7F]), Ok(127));
        assert_eq!(
            var(2, Encoding::Sle, false, 1).decode(&[0x00, 0x80]),
            Ok(-32768)
        );
        assert_eq!(
            var(2, Encoding::Sle, true, 1).decode(&[0x80, 0x00]),
            Ok(-32768)
        );
        let min = var(8, Encoding::Sle, false, 1).decode(&i64::MIN.to_le_bytes());
        assert_eq!(min, Ok(i64::MIN));
        let max = var(8, Encoding::Sle, false, 1).decode(&i64::MAX.to_le_bytes());
        assert_eq!(max, Ok(i64::MAX));
        let error = var(8, Encoding::Sle, false, 2)
            .decode(&(i64::MIN / 2 - 1).to_le_bytes())
            .unwrap_err();
        assert!(error.contains("doesn't fit"), "{error}");
    }

    #[test]
    fn validates_signed_range_with_scale() {
        let parse = |size: usize, scale: u32| {
            RamVar::try_from(RawRamVar {
                memory: None,
                offset: None,
                address: Some(0),
                size,
                encoding: Encoding::Sle,
                swap16: false,
                scale,
            })
        };
        assert!(parse(8, 1).is_ok(), "i64::MIN fits");
        assert!(parse(8, 2).unwrap_err().contains("doesn't fit an i64"));
        assert!(parse(4, 1 << 31).is_ok(), "-2^31 x 2^31 = -2^62");
        assert!(parse(4, u32::MAX).is_ok());
    }

    #[test]
    fn rejects_bad_bcd_and_overflow() {
        let error = var(2, Encoding::Bcd, false, 1)
            .decode(&[0x1A, 0])
            .unwrap_err();
        assert!(error.contains("0x1a isn't packed BCD"), "{error}");
        let error = var(2, Encoding::Digits, false, 1)
            .decode(&[1, 10])
            .unwrap_err();
        assert!(error.contains("0x0a isn't a decimal digit"), "{error}");
        let error = var(8, Encoding::Le, false, 1)
            .decode(&[0xFF; 8])
            .unwrap_err();
        assert!(error.contains("doesn't fit"), "{error}");
        let error = var(8, Encoding::Bcd, false, 1000)
            .decode(&[0x99; 8])
            .unwrap_err();
        assert!(error.contains("doesn't fit"), "{error}");
    }
}
