//! Sprite-list labels (docs/recording-format.md, "Sprite labels"): each frame's hardware
//! sprite table, decoded from the core's named state areas (docs/libretro.md, "Named areas")
//! into one record per sprite. They are training targets only, never a policy's input.
//!
//! [`SpriteStream::bind`] finds a board's areas once after the game is loaded and fails with
//! a reason when the core can't give them (a core without the area interface, or a game on
//! another board). [`SpriteStream::read`] then decodes, between frames, the list the *next*
//! frame draws: every board's decoder reads the buffer its driver draws from, so reading
//! before frame `f` labels frame `f`, whatever the observation rate.

mod psikyo;
mod testcore;

use gth_config::SpriteHw;
use gth_core::Core;

/// Bytes per record in a shard's sprite chunks: [`Sprite::encode`]'s layout.
pub const RECORD_SIZE: usize = 18;

/// [`Sprite::flags`]: the sprite is mirrored left to right.
pub const FLIP_X: u8 = 1 << 0;
/// [`Sprite::flags`]: the sprite is mirrored top to bottom.
pub const FLIP_Y: u8 = 1 << 1;
/// [`Sprite::flags`]: the box overlaps the screen. Boards keep sprites in their list that
/// are entirely off screen; their records stay, in list order, with this bit clear.
/// It says where the box is, not that anything shows: transparent tiles, or a sprite under a
/// higher tilemap, still count.
pub const VISIBLE: u8 = 1 << 2;
/// [`Sprite::flags`]: the board scaled the sprite, so `width`/`height` aren't `cols * 16` by
/// `rows * 16` (Psikyo's zoom).
pub const ZOOMED: u8 = 1 << 3;

/// One sprite as the frame draws it. Coordinates are the core's native screen pixels, before
/// any crop, rotation or resize ([`SpriteStream::screen`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sprite {
    /// The box's top-left corner; negative or past the screen when it hangs off an edge.
    pub x: i16,
    pub y: i16,
    /// The box's size in screen pixels, as drawn (after zoom).
    pub width: u16,
    pub height: u16,
    /// The board's tile code for the first tile (Psikyo: the 17-bit tile LUT index).
    pub code: u32,
    /// The board's palette number (Psikyo: 0-31).
    pub palette: u16,
    /// Tiles across and down (Psikyo: 1-8 each).
    pub cols: u8,
    pub rows: u8,
    /// The board's priority level (Psikyo: 0-3, FBNeo's layer order).
    pub priority: u8,
    /// [`FLIP_X`], [`FLIP_Y`], [`VISIBLE`], [`ZOOMED`].
    pub flags: u8,
}

impl Sprite {
    /// Appends the record: `x`, `y` (i16), `width`, `height` (u16), `code` (u32), `palette`
    /// (u16), `cols`, `rows`, `priority`, `flags` (u8), little-endian and packed.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.x.to_le_bytes());
        out.extend_from_slice(&self.y.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.code.to_le_bytes());
        out.extend_from_slice(&self.palette.to_le_bytes());
        out.extend_from_slice(&[self.cols, self.rows, self.priority, self.flags]);
    }

    /// Whether the box overlaps a `width` x `height` screen.
    fn overlaps(&self, [width, height]: [u32; 2]) -> bool {
        let (x, y) = (i64::from(self.x), i64::from(self.y));
        x < i64::from(width)
            && y < i64::from(height)
            && x + i64::from(self.width) > 0
            && y + i64::from(self.height) > 0
    }
}

/// Why a game has no sprite stream, or a read failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The core can't give the board's sprite list.
    #[error("no {hw} sprite stream from core {core}: {reason}")]
    Unavailable {
        hw: &'static str,
        core: String,
        reason: String,
    },
    /// The board's areas hold something its decoder doesn't expect.
    #[error("{hw} sprite list in core {core} is corrupt: {reason}")]
    Corrupt {
        hw: &'static str,
        core: String,
        reason: String,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A board's decoder, bound to a loaded core.
trait Board: Send {
    /// Appends the sprites the next frame draws to `out`, in the board's list order, with
    /// every field but [`VISIBLE`] set.
    fn read(&self, core: &mut Core, out: &mut Vec<Sprite>) -> Result<(), String>;

    /// `[width, height]` of the board's screen, which the coordinates are in.
    fn screen(&self) -> [u32; 2];
}

/// A game's sprite stream: its board's decoder and screen, bound to the loaded core.
pub struct SpriteStream {
    hw: SpriteHw,
    board: Box<dyn Board>,
    screen: [u32; 2],
}

impl SpriteStream {
    /// Binds `hw`'s decoder to `core`, which has loaded the game. Fails with
    /// [`Error::Unavailable`] if the core has no area interface (a stock core, or a fork
    /// older than the interface: docs/cores.md, "Forks"), lacks the board's areas, or draws
    /// a frame of another size than the board's screen (rotated).
    pub fn bind(hw: SpriteHw, core: &mut Core) -> Result<SpriteStream> {
        let unavailable = |core: &Core, reason: String| Error::Unavailable {
            hw: hw.name(),
            core: core_name(core),
            reason,
        };
        if !core.has_areas() {
            return Err(unavailable(
                core,
                "the core has no named-area interface (GTH_ENVIRONMENT_SET_AREA_INTERFACE: \
                 our FBNeo fork's, docs/libretro.md)"
                    .into(),
            ));
        }
        let board: Box<dyn Board> = match hw {
            SpriteHw::Psikyo => {
                Box::new(psikyo::Psikyo::bind(core).map_err(|r| unavailable(core, r))?)
            }
            SpriteHw::Testcore => {
                Box::new(testcore::Testcore::bind(core).map_err(|r| unavailable(core, r))?)
            }
        };
        // The coordinates are the board's; the frame must be drawn in the same space (not
        // rotated, as FBNeo's vertical mode would), or VISIBLE and any mapping would be off.
        let screen = board.screen();
        let av = core.av_info();
        if [av.base_width, av.base_height] != screen {
            return Err(unavailable(
                core,
                format!(
                    "the core draws {}x{}, not the board's {}x{} (rotated?)",
                    av.base_width, av.base_height, screen[0], screen[1]
                ),
            ));
        }
        Ok(SpriteStream { hw, board, screen })
    }

    pub fn hw(&self) -> SpriteHw {
        self.hw
    }

    /// `[width, height]` of the screen the coordinates are in: the board's, which
    /// [`SpriteStream::bind`] checked is the core's frame as it draws it, before the game's
    /// crop and orientation.
    pub fn screen(&self) -> [u32; 2] {
        self.screen
    }

    /// Replaces `out` with the sprites the next frame (the next `retro_run`) draws. Call it
    /// between frames; reading before frame `f` gives frame `f`'s list.
    pub fn read(&self, core: &mut Core, out: &mut Vec<Sprite>) -> Result<()> {
        out.clear();
        self.board
            .read(core, out)
            .map_err(|reason| Error::Corrupt {
                hw: self.hw.name(),
                core: core_name(core),
                reason,
            })?;
        for sprite in out.iter_mut() {
            sprite.flags &= !VISIBLE;
            if sprite.overlaps(self.screen) {
                sprite.flags |= VISIBLE;
            }
        }
        Ok(())
    }
}

fn core_name(core: &Core) -> String {
    let info = core.system_info();
    format!("{} {}", info.library_name, info.library_version)
}

/// Area `name` of `core`, checked to be `len` bytes.
fn area<'a>(core: &'a mut Core, name: &str, len: usize) -> Result<&'a [u8], String> {
    let area = core
        .area(name)
        .ok_or_else(|| format!("the core has no area \"{name}\""))?;
    if area.len() != len {
        return Err(format!(
            "area \"{name}\" is {} bytes, not {len}",
            area.len()
        ));
    }
    Ok(area)
}

fn i16_at(bytes: &[u8], at: usize) -> i16 {
    i16::from_ne_bytes([bytes[at], bytes[at + 1]])
}

fn i32_at(bytes: &[u8], at: usize) -> i32 {
    i32::from_ne_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_packed_little_endian() {
        let sprite = Sprite {
            x: -3,
            y: 0x0102,
            width: 32,
            height: 0x0304,
            code: 0x0001_2345,
            palette: 31,
            cols: 2,
            rows: 8,
            priority: 3,
            flags: FLIP_X | VISIBLE,
        };
        let mut out = Vec::new();
        sprite.encode(&mut out);
        assert_eq!(
            out,
            [
                0xFD, 0xFF, 0x02, 0x01, 32, 0, 0x04, 0x03, 0x45, 0x23, 0x01, 0, 31, 0, 2, 8, 3, 5
            ]
        );
        assert_eq!(out.len(), RECORD_SIZE);
    }

    #[test]
    fn visible_means_the_box_overlaps_the_screen() {
        let at = |x, y| Sprite {
            x,
            y,
            width: 16,
            height: 16,
            ..Sprite::default()
        };
        let screen = [320, 224];
        assert!(at(0, 0).overlaps(screen));
        assert!(at(-15, -15).overlaps(screen));
        assert!(at(319, 223).overlaps(screen));
        assert!(!at(-16, 0).overlaps(screen));
        assert!(!at(0, -16).overlaps(screen));
        assert!(!at(320, 0).overlaps(screen));
        assert!(!at(0, 224).overlaps(screen));
    }
}
