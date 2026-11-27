//! The test core's marker list (docs/libretro.md, "Test core"): a count byte, then up to 9
//! entries of x, y (i16), width, height (u8), little-endian. The core buffers it after each
//! frame and draws it in the next, as Psikyo does, so the full path runs in CI.

use gth_core::Core;

use crate::{Sprite, area};

const SPRITES: &str = "gth_testcore sprites";
const MAX: usize = 9;
const ENTRY_SIZE: usize = 6;
const SIZE: usize = 1 + MAX * ENTRY_SIZE;

pub(crate) struct Testcore;

impl Testcore {
    pub fn bind(core: &mut Core) -> Result<Testcore, String> {
        area(core, SPRITES, SIZE)?;
        Ok(Testcore)
    }
}

impl crate::Board for Testcore {
    fn read(&self, core: &mut Core, out: &mut Vec<Sprite>) -> Result<(), String> {
        let list = area(core, SPRITES, SIZE)?;
        let count = usize::from(list[0]);
        if count > MAX {
            return Err(format!("{count} sprites, more than {MAX}"));
        }
        out.extend(
            list[1..]
                .as_chunks::<ENTRY_SIZE>()
                .0
                .iter()
                .take(count)
                .enumerate()
                .map(|(i, entry)| Sprite {
                    x: i16::from_le_bytes([entry[0], entry[1]]),
                    y: i16::from_le_bytes([entry[2], entry[3]]),
                    width: u16::from(entry[4]),
                    height: u16::from(entry[5]),
                    code: i as u32,
                    cols: 1,
                    rows: 1,
                    ..Sprite::default()
                }),
        );
        Ok(())
    }

    fn screen(&self) -> [u32; 2] {
        [320, 240]
    }
}
