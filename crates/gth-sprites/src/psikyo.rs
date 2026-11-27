//! Psikyo's 68EC020 boards (FBNeo `src/burn/drv/psikyo/psikyo_sprite.cpp`).
//!
//! At each vblank the driver draws buffer `nFrame ^ 1`, then `PsikyoSpriteBuffer` flips
//! `nFrame` and fills buffer `nFrame` from sprite RAM. So between frames, buffer `nFrame ^ 1`
//! (filled the frame before) is the list the next frame draws, which is what we read. The
//! lists, the index and the ends are in savestates (our fork, docs/cores.md, "Forks"), so the
//! same holds right after a load.

use gth_core::Core;

use crate::{FLIP_X, FLIP_Y, Sprite, ZOOMED, area, i16_at, i32_at};

const LISTS: &str = "Psikyo sprite lists";
const INDEX: &str = "Psikyo sprite list index";
const ENDS: &str = "Psikyo sprite list ends";
/// Entries per buffer; two buffers.
const ENTRIES: usize = 0x400;
/// `sizeof(PsikyoSprite)`: i8 flip, i8 priority, i16 palette, then i32 x, y, xsize, ysize,
/// xzoom, yzoom and address.
const ENTRY_SIZE: usize = 32;
/// Four priority levels per buffer, each with its last entry (-1 for none).
const LEVELS: usize = 4;
/// The driver's screen, which the sprite coordinates are in (`DrvInit`'s 320x224).
const SCREEN: [u32; 2] = [320, 224];

pub(crate) struct Psikyo;

impl Psikyo {
    pub fn bind(core: &mut Core) -> Result<Psikyo, String> {
        area(core, LISTS, 2 * ENTRIES * ENTRY_SIZE)?;
        area(core, INDEX, 4)?;
        area(core, ENDS, 2 * LEVELS * 4)?;
        Ok(Psikyo)
    }
}

impl crate::Board for Psikyo {
    fn read(&self, core: &mut Core, out: &mut Vec<Sprite>) -> Result<(), String> {
        let [lists, index, ends] = core
            .areas([LISTS, INDEX, ENDS])
            .ok_or("the core no longer has its areas")?;
        read_lists(lists, index, ends, out)
    }

    fn screen(&self) -> [u32; 2] {
        SCREEN
    }
}

/// Appends the list the next frame draws, from the three areas' bytes.
fn read_lists(
    lists: &[u8],
    index: &[u8],
    ends: &[u8],
    out: &mut Vec<Sprite>,
) -> Result<(), String> {
    let buffer = ((i32_at(index, 0) & 1) ^ 1) as usize;
    // Entries 0..=last are this buffer's sprites in list order: every sprite gets a level,
    // so the highest level's last entry ends the list.
    let last = (0..LEVELS)
        .map(|level| i32_at(ends, 4 * (LEVELS * buffer + level)))
        .max()
        .unwrap_or(-1);
    if last >= ENTRIES as i32 {
        return Err(format!("buffer {buffer} ends at entry {last}"));
    }
    let count = usize::try_from(last + 1).unwrap_or(0);
    let entries = &lists[buffer * ENTRIES * ENTRY_SIZE..][..count * ENTRY_SIZE];
    out.extend(
        entries
            .as_chunks::<ENTRY_SIZE>()
            .0
            .iter()
            .map(|entry| decode(entry)),
    );
    Ok(())
}

/// One `PsikyoSprite`, with the box `PsikyoSpriteRender` draws it in.
fn decode(entry: &[u8]) -> Sprite {
    let flip = entry[0] & 3;
    let priority = entry[1].trailing_zeros().min(7) as u8;
    let palette = (i16_at(entry, 2) as u16) >> 4;
    let field = |i: usize| i32_at(entry, 4 + 4 * i);
    let (mut x, mut y, cols, rows) = (field(0), field(1), field(2), field(3));
    let (xzoom, yzoom, code) = (field(4), field(5), field(6));
    let zoomed = xzoom != 0 || yzoom != 0;
    // Unzoomed: 16-pixel tiles. Zoomed (both axes, even if one zoom is 0): the renderer
    // offsets the start and draws tile `i` (i + 1) * z / 2 - i * z / 2 pixels wide for
    // z = 32 - zoom, which sums to cols * z / 2.
    let (width, height) = if zoomed {
        x += (cols * xzoom + 3) >> 2;
        y += (rows * yzoom + 3) >> 2;
        (cols * (32 - xzoom) / 2, rows * (32 - yzoom) / 2)
    } else {
        (cols * 16, rows * 16)
    };
    let mut flags = 0;
    if flip & 1 != 0 {
        flags |= FLIP_X;
    }
    if flip & 2 != 0 {
        flags |= FLIP_Y;
    }
    if zoomed {
        flags |= ZOOMED;
    }
    Sprite {
        x: x as i16,
        y: y as i16,
        width: width as u16,
        height: height as u16,
        code: code as u32,
        palette,
        cols: cols as u8,
        rows: rows as u8,
        priority,
        flags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(flip: i8, priority: i8, palette: i16, fields: [i32; 7]) -> Vec<u8> {
        let mut out = vec![flip as u8, priority as u8];
        out.extend(palette.to_ne_bytes());
        for field in fields {
            out.extend(field.to_ne_bytes());
        }
        out
    }

    /// Two buffers whose entries are told apart by their x: buffer b's entry i has x
    /// `100 * b + i`.
    fn lists() -> Vec<u8> {
        let mut out = Vec::new();
        for buffer in 0..2 {
            for i in 0..ENTRIES as i32 {
                out.extend(entry(0, 1, 0, [100 * buffer + i, 0, 1, 1, 0, 0, 0]));
            }
        }
        out
    }

    fn ends(buffer0: [i32; 4], buffer1: [i32; 4]) -> Vec<u8> {
        buffer0
            .iter()
            .chain(&buffer1)
            .flat_map(|end| end.to_ne_bytes())
            .collect()
    }

    fn xs(n_frame: i32, ends: &[u8]) -> Result<Vec<i16>, String> {
        let mut out = Vec::new();
        read_lists(&lists(), &n_frame.to_ne_bytes(), ends, &mut out)?;
        Ok(out.iter().map(|s| s.x).collect())
    }

    /// The driver draws buffer `nFrame ^ 1` at the next vblank, so that's the one read, with
    /// as many entries as the highest level's end says.
    #[test]
    fn reads_the_buffer_the_next_frame_draws() {
        let both = ends([-1, 2, 0, -1], [1, -1, -1, 0]);
        assert_eq!(xs(0, &both).unwrap(), [100, 101]);
        assert_eq!(xs(1, &both).unwrap(), [0, 1, 2]);
        // Only nFrame's low bit counts.
        assert_eq!(xs(2, &both).unwrap(), [100, 101]);
        assert_eq!(xs(1, &ends([-1; 4], [5; 4])).unwrap(), [] as [i16; 0]);
        assert_eq!(xs(0, &ends([5; 4], [-1; 4])).unwrap(), [] as [i16; 0]);
        let full = ENTRIES as i32 - 1;
        assert_eq!(
            xs(1, &ends([full, -1, -1, -1], [-1; 4])).unwrap().len(),
            ENTRIES
        );
        let error = xs(1, &ends([-1, ENTRIES as i32, -1, -1], [-1; 4])).unwrap_err();
        assert_eq!(error, "buffer 0 ends at entry 1024");
    }

    #[test]
    fn decodes_an_unzoomed_sprite() {
        let sprite = decode(&entry(1, 1 << 2, 0x1F0, [-8, 200, 2, 3, 0, 0, 0x1_2345]));
        assert_eq!(
            sprite,
            Sprite {
                x: -8,
                y: 200,
                width: 32,
                height: 48,
                code: 0x1_2345,
                palette: 31,
                cols: 2,
                rows: 3,
                priority: 2,
                flags: FLIP_X,
            }
        );
    }

    #[test]
    fn decodes_a_zoomed_sprite_as_the_renderer_draws_it() {
        // xzoom 8: tiles 12 pixels wide, starting (2 * 8 + 3) / 4 = 4 pixels in; yzoom 0
        // keeps 16-pixel rows.
        let sprite = decode(&entry(3, 1, 0x010, [100, 50, 2, 1, 8, 0, 7]));
        assert_eq!((sprite.x, sprite.y), (104, 50));
        assert_eq!((sprite.width, sprite.height), (24, 16));
        assert_eq!(sprite.flags, FLIP_X | FLIP_Y | ZOOMED);
        assert_eq!((sprite.palette, sprite.priority), (1, 0));
        // Tile widths for z = 24: (i + 1) * 12 - i * 12, summing to cols * 12.
        let z = 32 - 8;
        let total: i32 = (0..2).map(|i| (i + 1) * z / 2 - i * z / 2).sum();
        assert_eq!(total, 24);
        // Odd z: the per-tile floors still sum to cols * z / 2.
        let z = 32 - 5;
        let total: i32 = (0..3).map(|i| (i + 1) * z / 2 - i * z / 2).sum();
        assert_eq!(total, 3 * z / 2);
    }
}
