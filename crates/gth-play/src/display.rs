//! The core's frame as the player shows it: the game's crop and tate rotation (as the
//! observations have them), at the core's own resolution, scaled by a whole number into the
//! window. The model's resolution is applied only at export (docs/human-play.md, "Display").

use gth_config::{GameConfig, Orientation};
use gth_frame::{FrameRef, Pipeline, Rect, Rotation};

/// Turns core frames into window pixels. Keeps its pipeline and buffer between frames, so
/// drawing allocates nothing while the frame size stays the same.
pub struct Screen {
    crop: Option<Rect>,
    rotation: Rotation,
    /// The pipeline for the last frame size, and that size (width, height).
    pipeline: Option<(Pipeline, (usize, usize))>,
    /// The rotated RGB8 frame.
    rgb: Vec<u8>,
}

/// The rotation for a game's orientation.
pub fn rotation(orientation: Orientation) -> Rotation {
    match orientation {
        Orientation::Deg0 => Rotation::None,
        Orientation::Deg90 => Rotation::Cw90,
        Orientation::Deg270 => Rotation::Cw270,
    }
}

impl Screen {
    pub fn new(game: &GameConfig) -> Screen {
        Screen {
            crop: game.crop.map(|c| Rect {
                x: c.x,
                y: c.y,
                width: c.width,
                height: c.height,
            }),
            rotation: rotation(game.orientation),
            pipeline: None,
            rgb: Vec::new(),
        }
    }

    /// The shown size (width, height) of a `width` x `height` core frame: cropped, then
    /// rotated.
    pub fn shown_size(&self, width: usize, height: usize) -> (usize, usize) {
        let (w, h) = self.crop.map_or((width, height), |c| (c.width, c.height));
        match self.rotation {
            Rotation::None => (w, h),
            Rotation::Cw90 | Rotation::Cw270 => (h, w),
        }
    }

    /// Converts `frame` (crop, rotation, RGB8). Returns its shown size.
    pub fn convert(&mut self, frame: FrameRef<'_>) -> Result<(usize, usize), String> {
        let size = (frame.width, frame.height);
        if self.pipeline.as_ref().map(|(_, s)| *s) != Some(size) {
            let (w, h) = self.shown_size(frame.width, frame.height);
            let pipeline = Pipeline::new(
                h,
                w,
                self.crop,
                self.rotation,
                gth_frame::Filter::Area,
                gth_frame::Color::Rgb,
            )
            .map_err(|e| e.to_string())?;
            self.rgb.resize(pipeline.out_len(), 0);
            self.pipeline = Some((pipeline, size));
        }
        let (pipeline, _) = self.pipeline.as_mut().expect("set above");
        pipeline
            .render(frame, &mut self.rgb)
            .map_err(|e| e.to_string())?;
        Ok((pipeline.out_width(), pipeline.out_height()))
    }

    /// Draws the last converted frame into `dst`, a `dst_w` x `dst_h` buffer of `0x00RRGGBB`
    /// pixels: scaled by the largest whole number that fits (at least 1, cut off if even
    /// that doesn't fit), centred on black. Returns the scale.
    pub fn draw(&self, dst: &mut [u32], dst_w: usize, dst_h: usize) -> usize {
        let Some((pipeline, _)) = &self.pipeline else {
            dst.fill(0);
            return 1;
        };
        blit(
            &self.rgb,
            pipeline.out_width(),
            pipeline.out_height(),
            dst,
            dst_w,
            dst_h,
        )
    }
}

/// The largest whole scale at which `w` x `h` fits in `max_w` x `max_h`, at least 1.
pub fn integer_scale(w: usize, h: usize, max_w: usize, max_h: usize) -> usize {
    if w == 0 || h == 0 {
        return 1;
    }
    (max_w / w).min(max_h / h).max(1)
}

/// Draws the `w` x `h` RGB8 image `rgb` into `dst` as [`Screen::draw`] does.
pub fn blit(rgb: &[u8], w: usize, h: usize, dst: &mut [u32], dst_w: usize, dst_h: usize) -> usize {
    assert_eq!(
        dst.len(),
        dst_w * dst_h,
        "dst must hold dst_w x dst_h pixels"
    );
    assert_eq!(rgb.len(), w * h * 3, "rgb must hold w x h RGB8 pixels");
    dst.fill(0);
    let scale = integer_scale(w, h, dst_w, dst_h);
    let (out_w, out_h) = (w * scale, h * scale);
    // Centred; negative offsets (an image larger than the window) cut off the edges.
    let x0 = (dst_w as isize - out_w as isize) / 2;
    let y0 = (dst_h as isize - out_h as isize) / 2;
    for y in 0..h {
        // The image row's dst rows that are in the window.
        let top = (y0 + (y * scale) as isize).clamp(0, dst_h as isize) as usize;
        let bottom = (y0 + ((y + 1) * scale) as isize).clamp(0, dst_h as isize) as usize;
        if top == bottom {
            continue;
        }
        let first = top;
        let line = &mut dst[first * dst_w..(first + 1) * dst_w];
        for (x, px) in rgb[y * w * 3..(y + 1) * w * 3]
            .as_chunks::<3>()
            .0
            .iter()
            .enumerate()
        {
            let value = u32::from(px[0]) << 16 | u32::from(px[1]) << 8 | u32::from(px[2]);
            let from = (x0 + (x * scale) as isize).clamp(0, dst_w as isize) as usize;
            let to = (x0 + ((x + 1) * scale) as isize).clamp(0, dst_w as isize) as usize;
            line[from..to].fill(value);
        }
        for dy in top + 1..bottom {
            dst.copy_within(first * dst_w..(first + 1) * dst_w, dy * dst_w);
        }
    }
    scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_are_whole_and_fit() {
        assert_eq!(integer_scale(240, 320, 1920, 1080), 3);
        assert_eq!(integer_scale(256, 240, 2560, 1440), 6);
        assert_eq!(integer_scale(320, 240, 100, 100), 1, "at least 1");
    }

    #[test]
    fn blit_scales_and_centres() {
        // 2x1: red, green.
        let rgb = [255, 0, 0, 0, 255, 0];
        let mut dst = vec![7; 5 * 4];
        assert_eq!(blit(&rgb, 2, 1, &mut dst, 5, 4), 2, "5 / 2 across");
        let (r, g) = (0xff0000, 0x00ff00);
        #[rustfmt::skip]
        let expected = [
            0, 0, 0, 0, 0,
            r, r, g, g, 0,
            r, r, g, g, 0,
            0, 0, 0, 0, 0,
        ];
        assert_eq!(dst, expected);
    }

    #[test]
    fn blit_cuts_off_what_does_not_fit() {
        let rgb: Vec<u8> = (0..4u8).flat_map(|i| [i, i, i]).collect(); // 4x1 grey ramp
        let mut dst = vec![9; 2];
        assert_eq!(blit(&rgb, 4, 1, &mut dst, 2, 1), 1);
        assert_eq!(dst, [0x010101, 0x020202], "the centre two");
    }

    #[test]
    fn shown_size_crops_then_rotates() {
        let game = GameConfig::parse(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../games/testcore/game.toml"),
            )
            .unwrap(),
            "testcore",
        )
        .unwrap();
        assert_eq!(Screen::new(&game).shown_size(320, 240), (232, 320));
    }
}
