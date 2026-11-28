//! [`Pipeline`]: a core's frame to a model observation (crop, RGB8, rotate, max-pool, gray,
//! resize).

use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use crate::{FrameRef, PixelFormat, RGB8_BYTES_PER_PIXEL, rgb8_len};

/// A rectangle of a source frame, in source pixels (before rotation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// How to turn the (cropped) frame, for games whose monitor is mounted vertically (tate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    None,
    /// 90 degrees clockwise.
    Cw90,
    /// 270 degrees clockwise (90 counter-clockwise).
    Cw270,
}

/// The resampling filter used when the rotated frame's size differs from the output's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// Averages the source pixels each output pixel covers (a box filter): the right choice
    /// when shrinking, and what OpenCV calls `INTER_AREA`.
    Area,
    Bilinear,
    /// Pillow's `BICUBIC`: the Catmull-Rom cubic (Keys, a = -0.5), widened by the scale when
    /// shrinking so it averages as well. OpenCV's `INTER_CUBIC` (a = -0.75, never widened)
    /// differs.
    Bicubic,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("output size {height}x{width} must be non-zero and fit in memory")]
    OutputSize { height: usize, width: usize },
    #[error("crop {crop:?} doesn't fit in the {width}x{height} frame")]
    Crop {
        crop: Rect,
        width: usize,
        height: usize,
    },
    #[error("frame {width}x{height} (pitch {pitch}) is empty or larger than its {len} bytes")]
    Frame {
        width: usize,
        height: usize,
        pitch: usize,
        len: usize,
    },
}

/// The channels of an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// RGB8: 3 bytes per pixel.
    Rgb,
    /// Luma, 1 byte per pixel: ITU-R BT.601's weights (0.299, 0.587, 0.114) in 16-bit fixed
    /// point, summing to exactly 1, so a gray pixel (r = g = b) keeps its value: a core that
    /// draws gray already (ALE's grayscale palette) passes through unchanged.
    Gray,
}

impl Color {
    /// Bytes per output pixel.
    pub const fn channels(self) -> usize {
        match self {
            Self::Rgb => RGB8_BYTES_PER_PIXEL,
            Self::Gray => 1,
        }
    }
}

/// BT.601 luma weights for R, G and B, out of `1 << 16`.
const LUMA: [u32; 3] = [19595, 38470, 7471];

/// The luma of an RGB8 pixel ([`Color::Gray`]), rounded.
pub fn luma([r, g, b]: [u8; 3]) -> u8 {
    let sum = LUMA[0] * u32::from(r) + LUMA[1] * u32::from(g) + LUMA[2] * u32::from(b);
    // The weights sum to 1 << 16, so the result is at most 255.
    ((sum + (1 << 15)) >> 16) as u8
}

/// Turns frames into `out_height` x `out_width` observations of [`Color`]: crop, convert to
/// RGB8 and rotate in one pass, optionally take the per-channel maximum with the previous
/// frame ([`render_max`](Self::render_max)), convert to gray, then resize. It keeps its
/// scratch buffers and resizer between frames, so rendering allocates nothing once the frame
/// size is stable.
pub struct Pipeline {
    out_height: usize,
    out_width: usize,
    crop: Option<Rect>,
    rotation: Rotation,
    color: Color,
    options: ResizeOptions,
    resizer: Resizer,
    /// The rotated RGB8 frame (pooled, with the previous frame); unused when it goes straight
    /// to the output.
    rgb: Vec<u8>,
    /// The rotated RGB8 previous frame, for [`render_max`](Self::render_max).
    previous: Vec<u8>,
    /// The rotated gray frame before resizing.
    gray: Vec<u8>,
}

impl Pipeline {
    /// Every choice is explicit: `crop` is `None` for the whole frame.
    pub fn new(
        out_height: usize,
        out_width: usize,
        crop: Option<Rect>,
        rotation: Rotation,
        filter: Filter,
        color: Color,
    ) -> Result<Pipeline, Error> {
        let fits = |n: usize| n > 0 && u32::try_from(n).is_ok();
        let len = rgb8_len(out_height, out_width).filter(|&len| isize::try_from(len).is_ok());
        if !fits(out_height) || !fits(out_width) || len.is_none() {
            return Err(Error::OutputSize {
                height: out_height,
                width: out_width,
            });
        }
        let filter = match filter {
            Filter::Area => FilterType::Box,
            Filter::Bilinear => FilterType::Bilinear,
            Filter::Bicubic => FilterType::CatmullRom,
        };
        Ok(Pipeline {
            out_height,
            out_width,
            crop,
            rotation,
            color,
            options: ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter)),
            resizer: Resizer::new(),
            rgb: Vec::new(),
            previous: Vec::new(),
            gray: Vec::new(),
        })
    }

    pub fn out_height(&self) -> usize {
        self.out_height
    }

    pub fn out_width(&self) -> usize {
        self.out_width
    }

    pub fn color(&self) -> Color {
        self.color
    }

    /// Bytes in one output frame (`out_height * out_width * channels`).
    pub fn out_len(&self) -> usize {
        self.out_height * self.out_width * self.color.channels()
    }

    /// Renders `frame` into `dst`, a row-major buffer of [`out_len`](Self::out_len) bytes
    /// (channels last). Fails if the crop doesn't fit the frame or the frame is empty or
    /// truncated; frames may change size between calls, so this is checked per frame.
    ///
    /// # Panics
    ///
    /// If `dst.len()` isn't `out_len()`: a bug in the caller.
    pub fn render(&mut self, frame: FrameRef<'_>, dst: &mut [u8]) -> Result<(), Error> {
        self.render_pooled(frame, None, dst)
    }

    /// Renders the per-channel maximum of `frame` and `previous`, the frame the core drew
    /// before it, as [`render`](Self::render) renders one frame (DQN's max-pooling, which
    /// removes the flicker of sprites drawn every other frame). The maximum is taken in RGB8
    /// before the gray conversion and the resize. A `previous` of another size or format
    /// than `frame` (the core changed resolution between them) is ignored.
    pub fn render_max(
        &mut self,
        frame: FrameRef<'_>,
        previous: FrameRef<'_>,
        dst: &mut [u8],
    ) -> Result<(), Error> {
        let same = (previous.width, previous.height, previous.format)
            == (frame.width, frame.height, frame.format);
        self.render_pooled(frame, same.then_some(previous), dst)
    }

    fn render_pooled(
        &mut self,
        frame: FrameRef<'_>,
        previous: Option<FrameRef<'_>>,
        dst: &mut [u8],
    ) -> Result<(), Error> {
        assert_eq!(dst.len(), self.out_len(), "dst must hold one output frame");
        let region = self.region(&frame)?;
        let previous = match previous {
            Some(previous) => Some((previous, self.region(&previous)?)),
            None => None,
        };
        let (width, height) = match self.rotation {
            Rotation::None => (region.width, region.height),
            Rotation::Cw90 | Rotation::Cw270 => (region.height, region.width),
        };
        let resize = (width, height) != (self.out_width, self.out_height);
        if !resize && self.color == Color::Rgb {
            self.rotate(&frame, previous, region, dst);
            return Ok(());
        }
        let pixels = width * height;
        // Taken out of `self` (no allocation) so `self` can be borrowed alongside.
        let mut rgb = std::mem::take(&mut self.rgb);
        rgb.resize(pixels * RGB8_BYTES_PER_PIXEL, 0);
        self.rotate(&frame, previous, region, &mut rgb);
        match self.color {
            Color::Rgb => self.resize(&rgb, width, height, PixelType::U8x3, dst),
            Color::Gray if !resize => to_gray(&rgb, dst),
            Color::Gray => {
                let mut gray = std::mem::take(&mut self.gray);
                gray.resize(pixels, 0);
                to_gray(&rgb, &mut gray);
                self.resize(&gray, width, height, PixelType::U8, dst);
                self.gray = gray;
            }
        }
        self.rgb = rgb;
        Ok(())
    }

    /// Crops, converts and rotates `frame` into `dst` as RGB8, taking the per-channel maximum
    /// with `previous` (and its region) if given.
    fn rotate(
        &mut self,
        frame: &FrameRef<'_>,
        previous: Option<(FrameRef<'_>, Rect)>,
        region: Rect,
        dst: &mut [u8],
    ) {
        convert(frame, region, self.rotation, dst);
        if let Some((previous, previous_region)) = previous {
            self.previous.resize(dst.len(), 0);
            convert(
                &previous,
                previous_region,
                self.rotation,
                &mut self.previous,
            );
            for (to, &from) in dst.iter_mut().zip(&self.previous) {
                *to = (*to).max(from);
            }
        }
    }

    /// Resizes `src`, a `width` x `height` frame of `pixel_type`, into `dst` at the output
    /// size.
    fn resize(
        &mut self,
        src: &[u8],
        width: usize,
        height: usize,
        pixel_type: PixelType,
        dst: &mut [u8],
    ) {
        // Sizes fit u32: the region is inside a frame whose sides are u32 in libretro, and
        // `new` checked the output. u8 buffers need no alignment.
        let src = ImageRef::new(width as u32, height as u32, src, pixel_type)
            .expect("src holds the rotated frame");
        let mut out = Image::from_slice_u8(
            self.out_width as u32,
            self.out_height as u32,
            dst,
            pixel_type,
        )
        .expect("dst holds one output frame");
        self.resizer
            .resize(&src, &mut out, &self.options)
            .expect("same pixel type, non-empty images");
    }

    /// The part of `frame` to keep, checked against the frame and its buffer.
    fn region(&self, frame: &FrameRef<'_>) -> Result<Rect, Error> {
        let frame_error = || Error::Frame {
            width: frame.width,
            height: frame.height,
            pitch: frame.pitch,
            len: frame.data.len(),
        };
        let row = frame
            .width
            .checked_mul(frame.format.bytes_per_pixel())
            .ok_or_else(frame_error)?;
        let needed = frame
            .height
            .checked_sub(1)
            .and_then(|rows| rows.checked_mul(frame.pitch))
            .and_then(|start| start.checked_add(row));
        if row == 0 || frame.pitch < row || needed.is_none_or(|needed| needed > frame.data.len()) {
            return Err(frame_error());
        }
        let whole = Rect {
            x: 0,
            y: 0,
            width: frame.width,
            height: frame.height,
        };
        let Some(crop) = self.crop else {
            return Ok(whole);
        };
        let inside = |start: usize, len: usize, limit: usize| {
            len > 0 && start.checked_add(len).is_some_and(|end| end <= limit)
        };
        if inside(crop.x, crop.width, frame.width) && inside(crop.y, crop.height, frame.height) {
            Ok(crop)
        } else {
            Err(Error::Crop {
                crop,
                width: frame.width,
                height: frame.height,
            })
        }
    }
}

/// Converts `region` of `frame` to RGB8 and rotates it into `dst` (sized for the result).
fn convert(frame: &FrameRef<'_>, region: Rect, rotation: Rotation, dst: &mut [u8]) {
    match frame.format {
        PixelFormat::Xrgb8888 => convert_with(frame, region, rotation, dst, |p: [u8; 4]| {
            let [_, r, g, b] = u32::from_ne_bytes(p).to_be_bytes();
            [r, g, b]
        }),
        PixelFormat::Rgb565 => convert_with(frame, region, rotation, dst, |p: [u8; 2]| {
            let p = u16::from_ne_bytes(p);
            [
                expand5((p >> 11) as u8),
                expand6((p >> 5) as u8 & 0x3F),
                expand5(p as u8 & 0x1F),
            ]
        }),
        PixelFormat::Rgb1555 => convert_with(frame, region, rotation, dst, |p: [u8; 2]| {
            let p = u16::from_ne_bytes(p);
            [
                expand5((p >> 10) as u8 & 0x1F),
                expand5((p >> 5) as u8 & 0x1F),
                expand5(p as u8 & 0x1F),
            ]
        }),
    }
}

/// 5 bits to 8, so that 0x1F becomes 0xFF.
fn expand5(v: u8) -> u8 {
    (v << 3) | (v >> 2)
}

/// 6 bits to 8, so that 0x3F becomes 0xFF.
fn expand6(v: u8) -> u8 {
    (v << 2) | (v >> 4)
}

/// Converts RGB8 `rgb` to [`luma`] in `gray` (a third of its length).
fn to_gray(rgb: &[u8], gray: &mut [u8]) {
    let (rgb, _) = rgb.as_chunks::<RGB8_BYTES_PER_PIXEL>();
    for (to, &from) in gray.iter_mut().zip(rgb) {
        *to = luma(from);
    }
}

/// [`convert`] for one source format, monomorphized so the per-pixel work inlines.
fn convert_with<const BPP: usize>(
    frame: &FrameRef<'_>,
    region: Rect,
    rotation: Rotation,
    dst: &mut [u8],
    to_rgb: impl Fn([u8; BPP]) -> [u8; 3],
) {
    let (w, h) = (region.width, region.height);
    let (out, _) = dst.as_chunks_mut::<RGB8_BYTES_PER_PIXEL>();
    for y in 0..h {
        let start = (region.y + y) * frame.pitch + region.x * BPP;
        let (row, _) = frame.data[start..start + w * BPP].as_chunks::<BPP>();
        match rotation {
            Rotation::None => {
                for (to, &from) in out[y * w..][..w].iter_mut().zip(row) {
                    *to = to_rgb(from);
                }
            }
            // Source (x, y) lands in column h-1-y, row x of the h-wide result.
            Rotation::Cw90 => {
                for (x, &from) in row.iter().enumerate() {
                    out[x * h + (h - 1 - y)] = to_rgb(from);
                }
            }
            // Source (x, y) lands in column y, row w-1-x.
            Rotation::Cw270 => {
                for (x, &from) in row.iter().enumerate() {
                    out[(w - 1 - x) * h + y] = to_rgb(from);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of XRGB8888 pixels with `pad` unused bytes after each row.
    fn xrgb(pixels: &[&[u32]], pad: usize) -> (Vec<u8>, usize, usize, usize) {
        let (height, width) = (pixels.len(), pixels[0].len());
        let pitch = width * 4 + pad;
        let mut data = vec![0xAA; pitch * height];
        for (y, row) in pixels.iter().enumerate() {
            for (x, p) in row.iter().enumerate() {
                data[y * pitch + x * 4..][..4].copy_from_slice(&p.to_ne_bytes());
            }
        }
        (data, width, height, pitch)
    }

    fn render(
        frame: FrameRef<'_>,
        out: (usize, usize),
        crop: Option<Rect>,
        rotation: Rotation,
    ) -> Result<Vec<u8>, Error> {
        let mut pipeline = Pipeline::new(out.0, out.1, crop, rotation, Filter::Area, Color::Rgb)?;
        let mut dst = vec![0; pipeline.out_len()];
        pipeline.render(frame, &mut dst)?;
        Ok(dst)
    }

    /// 2x3 pixels whose red channel is the pixel's index, row-major.
    const GRID: [&[u32]; 2] = [
        &[0x00_0000, 0x01_0000, 0x02_0000],
        &[0x03_0000, 0x04_0000, 0x05_0000],
    ];

    fn reds(rgb: &[u8]) -> Vec<u8> {
        rgb.iter().step_by(3).copied().collect()
    }

    fn grid() -> (Vec<u8>, usize, usize, usize) {
        xrgb(&GRID, 4)
    }

    fn frame_of(data: &[u8], width: usize, height: usize, pitch: usize) -> FrameRef<'_> {
        FrameRef {
            data,
            width,
            height,
            pitch,
            format: PixelFormat::Xrgb8888,
        }
    }

    #[test]
    fn converts_the_three_formats() {
        let one = |data: &[u8], format| {
            let frame = FrameRef {
                data,
                width: 1,
                height: 1,
                pitch: data.len(),
                format,
            };
            render(frame, (1, 1), None, Rotation::None)
        };
        assert_eq!(
            one(&0x0012_3456u32.to_ne_bytes(), PixelFormat::Xrgb8888),
            Ok(vec![0x12, 0x34, 0x56])
        );
        // RGB565: r=31, g=32, b=1.
        let p = (31u16 << 11) | (32 << 5) | 1;
        assert_eq!(
            one(&p.to_ne_bytes(), PixelFormat::Rgb565),
            Ok(vec![0xFF, 0x82, 0x08])
        );
        // 0RGB1555: r=1, g=31, b=16.
        let p = (1u16 << 10) | (31 << 5) | 16;
        assert_eq!(
            one(&p.to_ne_bytes(), PixelFormat::Rgb1555),
            Ok(vec![0x08, 0xFF, 0x84])
        );
    }

    #[test]
    fn skips_row_padding() {
        let (data, w, h, pitch) = grid();
        let out = render(frame_of(&data, w, h, pitch), (2, 3), None, Rotation::None).unwrap();
        assert_eq!(reds(&out), [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn rotates_both_ways() {
        let (data, w, h, pitch) = grid();
        let frame = frame_of(&data, w, h, pitch);
        // 0 1 2      3 0          2 5
        // 3 4 5  ->  4 1 (cw90),  1 4 (cw270)
        //            5 2          0 3
        let cw90 = render(frame, (3, 2), None, Rotation::Cw90).unwrap();
        assert_eq!(reds(&cw90), [3, 0, 4, 1, 5, 2]);
        let cw270 = render(frame, (3, 2), None, Rotation::Cw270).unwrap();
        assert_eq!(reds(&cw270), [2, 5, 1, 4, 0, 3]);
    }

    #[test]
    fn crops_before_rotating() {
        let (data, w, h, pitch) = grid();
        let frame = frame_of(&data, w, h, pitch);
        let crop = Rect {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        };
        let out = render(frame, (2, 2), Some(crop), Rotation::None).unwrap();
        assert_eq!(reds(&out), [1, 2, 4, 5]);
        let out = render(frame, (2, 2), Some(crop), Rotation::Cw90).unwrap();
        assert_eq!(reds(&out), [4, 1, 5, 2]);
    }

    #[test]
    fn resizes_with_area_and_bilinear() {
        // 4x4 of 2x2 blocks: 0, 40 / 80, 120 in red.
        let block = |y: usize, x: usize| (((y / 2) * 2 + x / 2) as u32 * 40) << 16;
        let rows: Vec<Vec<u32>> = (0..4)
            .map(|y| (0..4).map(|x| block(y, x)).collect())
            .collect();
        let rows: Vec<&[u32]> = rows.iter().map(Vec::as_slice).collect();
        let (data, w, h, pitch) = xrgb(&rows, 0);
        let out = render(frame_of(&data, w, h, pitch), (2, 2), None, Rotation::None).unwrap();
        assert_eq!(reds(&out), [0, 40, 80, 120], "area averages each block");
        let mut pipeline =
            Pipeline::new(8, 8, None, Rotation::None, Filter::Bilinear, Color::Rgb).unwrap();
        let mut big = vec![0; pipeline.out_len()];
        pipeline
            .render(frame_of(&data, w, h, pitch), &mut big)
            .unwrap();
        assert_eq!(big[0], 0);
        assert_eq!(big[(8 * 8 - 1) * 3], 120);
    }

    #[test]
    fn bicubic_matches_pillow() {
        // An 8x6 frame; pixel (y, x) is [(37x + 11y) % 256, (7x^2 + 29y) % 256, 31(x ^ y) % 256].
        let rows: Vec<Vec<u32>> = (0..6u32)
            .map(|y| {
                (0..8u32)
                    .map(|x| {
                        let r = (37 * x + 11 * y) % 256;
                        let g = (7 * x * x + 29 * y) % 256;
                        (r << 16) | (g << 8) | ((31 * (x ^ y)) % 256)
                    })
                    .collect()
            })
            .collect();
        let rows: Vec<&[u32]> = rows.iter().map(Vec::as_slice).collect();
        let (data, w, h, pitch) = xrgb(&rows, 0);
        // Pillow 10.4's BICUBIC in float (mode "F", per channel, clipped to 0..=255): the
        // exact two-pass result. Ours rounds once between the passes, so a channel may be up
        // to 1 off; Pillow's own 8-bit path rounds more and is up to 2.1 off.
        let pillow: [((usize, usize), &[f32]); 2] = [
            (
                (2, 3),
                &[
                    45.6, 40.5, 34.5, 149.0, 124.3, 108.5, 158.1, 132.5, 182.5, 75.8, 126.5, 124.2,
                    176.7, 139.9, 108.5, 120.8, 121.4, 92.8,
                ],
            ),
            (
                (4, 10),
                &[
                    1.6, 8.0, 8.6, 27.1, 11.9, 9.7, 58.7, 24.1, 46.5, 88.3, 45.4, 81.7, 117.9,
                    75.6, 87.5, 147.5, 114.7, 129.5, 177.1, 171.2, 135.3, 222.7, 187.6, 170.5,
                    166.3, 149.6, 207.3, 0.0, 92.0, 208.4, 17.2, 49.3, 52.9, 42.8, 53.1, 56.8,
                    74.3, 65.3, 41.4, 103.9, 86.6, 20.9, 133.5, 116.9, 39.9, 163.1, 156.5, 177.1,
                    191.9, 214.1, 196.1, 244.3, 121.2, 175.6, 190.3, 51.4, 160.2, 11.9, 140.9,
                    164.1, 34.6, 95.1, 102.1, 60.2, 99.0, 101.9, 91.7, 111.2, 87.9, 121.3, 132.2,
                    71.7, 150.9, 164.5, 78.1, 180.5, 221.4, 138.9, 216.4, 72.5, 145.3, 207.6, 41.2,
                    129.1, 131.8, 121.6, 115.1, 33.4, 186.4, 114.9, 50.2, 136.3, 146.4, 75.8,
                    140.2, 135.6, 107.3, 152.4, 176.5, 136.9, 179.0, 224.0, 166.5, 193.6, 194.1,
                    196.1, 89.7, 22.9, 245.5, 51.1, 0.0, 120.3, 85.7, 40.5, 2.8, 163.8, 81.4, 57.0,
                    227.6, 70.6,
                ],
            ),
        ];
        for ((height, width), expected) in pillow {
            let mut pipeline = Pipeline::new(
                height,
                width,
                None,
                Rotation::None,
                Filter::Bicubic,
                Color::Rgb,
            )
            .unwrap();
            let mut out = vec![0; pipeline.out_len()];
            pipeline
                .render(frame_of(&data, w, h, pitch), &mut out)
                .unwrap();
            assert_eq!(out.len(), expected.len());
            for (i, (&ours, &exact)) in out.iter().zip(expected).enumerate() {
                assert!(
                    (f32::from(ours) - exact).abs() <= 1.0,
                    "{height}x{width}[{i}]: {ours}"
                );
            }
        }
    }

    #[test]
    fn luma_keeps_gray_pixels_exactly() {
        for v in 0..=255 {
            assert_eq!(luma([v, v, v]), v);
        }
        // 0.299, 0.587 and 0.114 of 255, rounded.
        assert_eq!(luma([255, 0, 0]), 76);
        assert_eq!(luma([0, 255, 0]), 150);
        assert_eq!(luma([0, 0, 255]), 29);
        assert_eq!(luma([10, 20, 30]), 18);
    }

    fn pipeline(
        out: (usize, usize),
        crop: Option<Rect>,
        rotation: Rotation,
        color: Color,
    ) -> Pipeline {
        Pipeline::new(out.0, out.1, crop, rotation, Filter::Area, color).unwrap()
    }

    #[test]
    fn gray_converts_before_resizing() {
        let (data, w, h, pitch) = xrgb(
            &[&[0x00FF_0000, 0x0000_FF00], &[0x0000_00FF, 0x0050_5050]],
            0,
        );
        let frame = frame_of(&data, w, h, pitch);
        let mut same = pipeline((2, 2), None, Rotation::None, Color::Gray);
        assert_eq!(same.out_len(), 4);
        let mut out = vec![0; 4];
        same.render(frame, &mut out).unwrap();
        assert_eq!(out, [76, 150, 29, 0x50]);
        let mut one = pipeline((1, 1), None, Rotation::None, Color::Gray);
        let mut out = vec![0; 1];
        one.render(frame, &mut out).unwrap();
        // The area average of the four lumas, (76 + 150 + 29 + 80) / 4, rounded.
        assert_eq!(out, [84]);
        let mut turned = pipeline((2, 2), None, Rotation::Cw90, Color::Gray);
        let mut out = vec![0; 4];
        turned.render(frame, &mut out).unwrap();
        assert_eq!(out, [29, 76, 0x50, 150]);
    }

    #[test]
    fn max_pools_each_channel_before_gray() {
        let (now, w, h, pitch) = xrgb(&[&[0x00C8_0010, 0x0000_0000]], 0);
        let (before, ..) = xrgb(&[&[0x0000_C820, 0x0001_0203]], 0);
        let (now, before) = (frame_of(&now, w, h, pitch), frame_of(&before, w, h, pitch));
        let mut rgb = pipeline((1, 2), None, Rotation::None, Color::Rgb);
        let mut out = vec![0; 6];
        rgb.render_max(now, before, &mut out).unwrap();
        assert_eq!(out, [0xC8, 0xC8, 0x20, 1, 2, 3]);
        // Gray of the maximum (200, 200, 32), not the maximum of the two grays (60 and 121).
        let mut gray = pipeline((1, 2), None, Rotation::None, Color::Gray);
        let mut out = vec![0; 2];
        gray.render_max(now, before, &mut out).unwrap();
        assert_eq!(out[0], luma([200, 200, 32]));
        assert_eq!(out[0], 181);
        // Pooled at the source size, then resized: the area average of the two maxima.
        let mut small = pipeline((1, 1), None, Rotation::None, Color::Rgb);
        let mut out = vec![0; 3];
        small.render_max(now, before, &mut out).unwrap();
        assert_eq!(out, [101, 101, 18]);
    }

    #[test]
    fn max_pools_the_cropped_turned_region() {
        let (data, w, h, pitch) = grid();
        let frame = frame_of(&data, w, h, pitch);
        // The same grid with red 10 - index: the maximum is 10 - i for i < 5, else i.
        let rows: Vec<Vec<u32>> = GRID
            .iter()
            .map(|row| row.iter().map(|p| (10 - (p >> 16)) << 16).collect())
            .collect();
        let rows: Vec<&[u32]> = rows.iter().map(Vec::as_slice).collect();
        let (other, ..) = xrgb(&rows, 4);
        let previous = frame_of(&other, w, h, pitch);
        let crop = Some(Rect {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        });
        let mut pipeline = pipeline((2, 2), crop, Rotation::Cw90, Color::Rgb);
        let mut out = vec![0; 12];
        pipeline.render_max(frame, previous, &mut out).unwrap();
        // Region 1 2 / 4 5 pooled to 9 8 / 6 5, turned clockwise: 6 9 / 5 8.
        assert_eq!(reds(&out), [6, 9, 5, 8]);
    }

    #[test]
    fn max_pool_ignores_a_previous_frame_of_another_size() {
        let (data, w, h, pitch) = grid();
        let frame = frame_of(&data, w, h, pitch);
        let (small, sw, sh, spitch) = xrgb(&[&[0x00FF_FFFF]], 0);
        let mut pipeline = pipeline((2, 3), None, Rotation::None, Color::Rgb);
        let mut out = vec![0; 18];
        pipeline
            .render_max(frame, frame_of(&small, sw, sh, spitch), &mut out)
            .unwrap();
        assert_eq!(reds(&out), [0, 1, 2, 3, 4, 5]);
        // A truncated previous frame of the same size is an error, as for the frame itself.
        let truncated = frame_of(&data[..data.len() - 5], w, h, pitch);
        assert!(matches!(
            pipeline.render_max(frame, truncated, &mut out),
            Err(Error::Frame { .. })
        ));
    }

    #[test]
    fn rejects_bad_crops_frames_and_sizes() {
        let (data, w, h, pitch) = grid();
        let frame = frame_of(&data, w, h, pitch);
        let crop = Rect {
            x: 2,
            y: 0,
            width: 2,
            height: 1,
        };
        assert!(matches!(
            render(frame, (1, 2), Some(crop), Rotation::None),
            Err(Error::Crop { .. })
        ));
        let empty = Rect { width: 0, ..crop };
        assert!(render(frame, (1, 1), Some(empty), Rotation::None).is_err());
        let truncated = frame_of(&data[..data.len() - 5], w, h, pitch);
        assert!(matches!(
            render(truncated, (2, 3), None, Rotation::None),
            Err(Error::Frame { .. })
        ));
        assert!(render(frame_of(&[], 0, 0, 0), (1, 1), None, Rotation::None).is_err());
        assert!(matches!(
            Pipeline::new(0, 4, None, Rotation::None, Filter::Area, Color::Gray),
            Err(Error::OutputSize { .. })
        ));
        assert!(
            Pipeline::new(
                usize::MAX,
                4,
                None,
                Rotation::None,
                Filter::Area,
                Color::Rgb
            )
            .is_err()
        );
    }
}
