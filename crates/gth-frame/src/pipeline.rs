//! [`Pipeline`]: a core's frame to a model observation (crop, RGB8, rotate, resize).

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

/// Turns frames into `out_height` x `out_width` RGB8 observations: crop, convert to RGB8 and
/// rotate in one pass, then resize. It keeps its scratch buffer and resizer between frames,
/// so rendering allocates nothing once the frame size is stable.
pub struct Pipeline {
    out_height: usize,
    out_width: usize,
    crop: Option<Rect>,
    rotation: Rotation,
    options: ResizeOptions,
    resizer: Resizer,
    /// The rotated RGB8 frame before resizing; unused when it already has the output size.
    scratch: Vec<u8>,
}

impl Pipeline {
    /// Every choice is explicit: `crop` is `None` for the whole frame.
    pub fn new(
        out_height: usize,
        out_width: usize,
        crop: Option<Rect>,
        rotation: Rotation,
        filter: Filter,
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
        };
        Ok(Pipeline {
            out_height,
            out_width,
            crop,
            rotation,
            options: ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter)),
            resizer: Resizer::new(),
            scratch: Vec::new(),
        })
    }

    pub fn out_height(&self) -> usize {
        self.out_height
    }

    pub fn out_width(&self) -> usize {
        self.out_width
    }

    /// Bytes in one output frame (`out_height * out_width * 3`).
    pub fn out_len(&self) -> usize {
        self.out_height * self.out_width * RGB8_BYTES_PER_PIXEL
    }

    /// Renders `frame` into `dst`, a row-major RGB8 buffer of [`out_len`](Self::out_len) bytes.
    /// Fails if the crop doesn't fit the frame or the frame is empty or truncated; frames may
    /// change size between calls, so this is checked per frame.
    ///
    /// # Panics
    ///
    /// If `dst.len()` isn't `out_len()`: a bug in the caller.
    pub fn render(&mut self, frame: FrameRef<'_>, dst: &mut [u8]) -> Result<(), Error> {
        assert_eq!(dst.len(), self.out_len(), "dst must hold one output frame");
        let region = self.region(&frame)?;
        let (width, height) = match self.rotation {
            Rotation::None => (region.width, region.height),
            Rotation::Cw90 | Rotation::Cw270 => (region.height, region.width),
        };
        if (width, height) == (self.out_width, self.out_height) {
            convert(&frame, region, self.rotation, dst);
            return Ok(());
        }
        self.scratch
            .resize(width * height * RGB8_BYTES_PER_PIXEL, 0);
        convert(&frame, region, self.rotation, &mut self.scratch);
        // Sizes fit u32: the region is inside a frame whose sides are u32 in libretro, and
        // `new` checked the output. RGB8 buffers need no alignment.
        let src = ImageRef::new(width as u32, height as u32, &self.scratch, PixelType::U8x3)
            .expect("scratch holds the rotated frame");
        let mut out = Image::from_slice_u8(
            self.out_width as u32,
            self.out_height as u32,
            dst,
            PixelType::U8x3,
        )
        .expect("dst holds one output frame");
        self.resizer
            .resize(&src, &mut out, &self.options)
            .expect("same pixel type, non-empty images");
        Ok(())
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
        let mut pipeline = Pipeline::new(out.0, out.1, crop, rotation, Filter::Area)?;
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
        let mut pipeline = Pipeline::new(8, 8, None, Rotation::None, Filter::Bilinear).unwrap();
        let mut big = vec![0; pipeline.out_len()];
        pipeline
            .render(frame_of(&data, w, h, pitch), &mut big)
            .unwrap();
        assert_eq!(big[0], 0);
        assert_eq!(big[(8 * 8 - 1) * 3], 120);
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
            Pipeline::new(0, 4, None, Rotation::None, Filter::Area),
            Err(Error::OutputSize { .. })
        ));
        assert!(Pipeline::new(usize::MAX, 4, None, Rotation::None, Filter::Area).is_err());
    }
}
