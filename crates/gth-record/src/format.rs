//! The binary layouts of `inputs.bin` and `checks.bin` (both zstd-compressed on disk), and
//! the state hashes a check holds. All integers are little-endian.

use std::collections::BTreeMap;
use std::path::Path;

use gth_config::{MemoryId, RamVar};
use gth_core::Core;
use gth_frame::PixelFormat;
use sha1::{Digest, Sha1};

use crate::{Result, format_error};

pub(crate) const INPUTS_MAGIC: &[u8; 8] = b"GTHINPUT";
pub(crate) const CHECKS_MAGIC: &[u8; 8] = b"GTHCHECK";
const INPUT_SIZE: usize = 3;
/// Flags bit 0: this frame ends an agent step.
const STEP_END: u8 = 1;

/// One emulated frame of `inputs.bin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Input {
    /// Port 0's RetroPad mask (bit n = `RETRO_DEVICE_ID_JOYPAD_*` n).
    pub buttons: u16,
    /// Whether this is the last frame of an agent step (the frame its observation shows).
    pub step_end: bool,
}

impl Input {
    pub(crate) fn encode(self) -> [u8; INPUT_SIZE] {
        let [lo, hi] = self.buttons.to_le_bytes();
        [lo, hi, if self.step_end { STEP_END } else { 0 }]
    }

    /// Decodes `inputs.bin` (uncompressed), checking its magic and flags.
    pub(crate) fn decode_all(data: &[u8], path: &Path) -> Result<Vec<Input>> {
        let body = data
            .strip_prefix(INPUTS_MAGIC)
            .ok_or_else(|| format_error(path, "inputs.bin doesn't start with GTHINPUT"))?;
        let (records, rest) = body.as_chunks::<INPUT_SIZE>();
        if !rest.is_empty() {
            return Err(format_error(path, "inputs.bin ends in a partial frame"));
        }
        records
            .iter()
            .enumerate()
            .map(|(i, &[lo, hi, flags])| {
                if flags & !STEP_END != 0 {
                    return Err(format_error(
                        path,
                        format!("inputs.bin frame {} has unknown flags {flags:#x}", i + 1),
                    ));
                }
                Ok(Input {
                    buttons: u16::from_le_bytes([lo, hi]),
                    step_end: flags & STEP_END != 0,
                })
            })
            .collect()
    }
}

/// One record of `checks.bin`: the state after `frame` frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Frames run since the start state (1-based: the check after the first frame is 1).
    pub frame: u64,
    /// Truncated SHA-1 of the frame (size, pixel format and pixels).
    pub frame_hash: u64,
    /// Truncated SHA-1 of the core's system RAM (of nothing if it exposes none).
    pub ram_hash: u64,
    /// Each RAM variable of the game, in name order; `None` when its bytes didn't decode.
    pub ram: Vec<Option<i64>>,
}

fn truncated(hasher: Sha1) -> u64 {
    let digest = hasher.finalize();
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-1 has 20 bytes"))
}

impl Check {
    /// The core's state now, after `frame` frames.
    pub(crate) fn take(core: &mut Core, frame: u64, vars: &BTreeMap<String, RamVar>) -> Check {
        let frame_hash = core.with_frame(|f| {
            let mut hasher = Sha1::new();
            hasher.update((f.width as u64).to_le_bytes());
            hasher.update((f.height as u64).to_le_bytes());
            // Fixed codes, so reordering the enum can't change recorded hashes.
            hasher.update([match f.format {
                PixelFormat::Rgb1555 => 0u8,
                PixelFormat::Xrgb8888 => 1,
                PixelFormat::Rgb565 => 2,
            }]);
            hasher.update(f.data);
            truncated(hasher)
        });
        let mut hasher = Sha1::new();
        hasher.update(core.memory(MemoryId::SystemRam.id()).unwrap_or(&[]));
        let ram_hash = truncated(hasher);
        let ram = vars
            .iter()
            .map(|(name, var)| core.read_ram(name, var).ok())
            .collect();
        Check {
            frame,
            frame_hash,
            ram_hash,
            ram,
        }
    }

    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        out.extend(self.frame.to_le_bytes());
        out.extend(self.frame_hash.to_le_bytes());
        out.extend(self.ram_hash.to_le_bytes());
        for value in &self.ram {
            out.push(u8::from(value.is_some()));
            out.extend(value.unwrap_or(0).to_le_bytes());
        }
    }

    /// The header of `checks.bin`: magic and the number of RAM variables per check.
    pub(crate) fn header(vars: usize) -> Vec<u8> {
        let mut header = CHECKS_MAGIC.to_vec();
        header.extend(
            u32::try_from(vars)
                .expect("few RAM variables")
                .to_le_bytes(),
        );
        header
    }

    /// Decodes `checks.bin` (uncompressed) for a game with `vars` RAM variables.
    pub(crate) fn decode_all(data: &[u8], vars: usize, path: &Path) -> Result<Vec<Check>> {
        let bad = |reason: &str| format_error(path, format!("checks.bin {reason}"));
        let body = data
            .strip_prefix(CHECKS_MAGIC)
            .ok_or_else(|| bad("doesn't start with GTHCHECK"))?;
        let (count, body) = body
            .split_first_chunk::<4>()
            .ok_or_else(|| bad("is short"))?;
        if u32::from_le_bytes(*count) as usize != vars {
            return Err(bad("has another number of RAM variables than game.toml"));
        }
        let size = 24 + 9 * vars;
        if body.len() % size != 0 {
            return Err(bad("ends in a partial check"));
        }
        let u64_at =
            |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"));
        let checks: Vec<Check> = body
            .chunks_exact(size)
            .map(|record| {
                let ram = record[24..]
                    .as_chunks::<9>()
                    .0
                    .iter()
                    .map(|v| (v[0] != 0).then(|| u64_at(v, 1) as i64))
                    .collect();
                Check {
                    frame: u64_at(record, 0),
                    frame_hash: u64_at(record, 8),
                    ram_hash: u64_at(record, 16),
                    ram,
                }
            })
            .collect();
        if checks.windows(2).any(|w| w[0].frame >= w[1].frame)
            || checks.first().is_some_and(|c| c.frame == 0)
        {
            return Err(bad("has check frames out of order"));
        }
        Ok(checks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_round_trip_and_reject_unknown_flags() {
        let inputs = [
            Input {
                buttons: 0x8001,
                step_end: false,
            },
            Input {
                buttons: 0,
                step_end: true,
            },
        ];
        let mut data = INPUTS_MAGIC.to_vec();
        inputs.iter().for_each(|i| data.extend(i.encode()));
        let path = Path::new("r.gthrec");
        assert_eq!(Input::decode_all(&data, path).unwrap(), inputs);
        data[INPUTS_MAGIC.len() + 2] = 0x80;
        let error = Input::decode_all(&data, path).unwrap_err().to_string();
        assert!(error.contains("frame 1 has unknown flags 0x80"), "{error}");
        assert!(Input::decode_all(&data[..data.len() - 1], path).is_err());
        assert!(Input::decode_all(b"GTHCHECK", path).is_err());
    }

    #[test]
    fn checks_round_trip_and_reject_disorder() {
        let checks = [
            Check {
                frame: 60,
                frame_hash: 1,
                ram_hash: 2,
                ram: vec![Some(-5), None],
            },
            Check {
                frame: 70,
                frame_hash: 3,
                ram_hash: 4,
                ram: vec![Some(7), Some(0)],
            },
        ];
        let mut data = Check::header(2);
        checks.iter().for_each(|c| c.encode(&mut data));
        let path = Path::new("r.gthrec");
        assert_eq!(Check::decode_all(&data, 2, path).unwrap(), checks);
        assert!(Check::decode_all(&data, 1, path).is_err());
        assert!(Check::decode_all(&data[..data.len() - 1], 2, path).is_err());
        let mut disordered = Check::header(2);
        checks.iter().rev().for_each(|c| c.encode(&mut disordered));
        let error = Check::decode_all(&disordered, 2, path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("out of order"), "{error}");
    }
}
