//! Frames to numpy arrays: Rust fills a buffer, then moves it into the array without copying.

use numpy::ndarray::{Array3, Array4};
use numpy::{IntoPyArray, PyArray3, PyArray4};
use pyo3::exceptions::{PyMemoryError, PyValueError};
use pyo3::prelude::*;

use gth_frame::RGB8_BYTES_PER_PIXEL;

use crate::errors;

/// A zeroed buffer for a `height` x `width` frame of `channels` bytes per pixel. Python's size
/// arguments are untrusted, so a size that overflows or can't be allocated raises instead of
/// aborting the process.
pub(crate) fn frame_buffer(
    height: usize,
    width: usize,
    channels: usize,
) -> errors::Result<Vec<u8>> {
    frames_buffer(1, height, width, channels)
}

/// A zeroed buffer for `frames` frames of `height` x `width` x `channels`, checked like
/// [`frame_buffer`].
pub(crate) fn frames_buffer(
    frames: usize,
    height: usize,
    width: usize,
    channels: usize,
) -> errors::Result<Vec<u8>> {
    let len = height
        .checked_mul(width)
        .and_then(|len| len.checked_mul(channels))
        .and_then(|len| len.checked_mul(frames));
    zeroed(len, || {
        if frames == 1 {
            format!("frame {height}x{width}x{channels} is too large")
        } else {
            format!("{frames} frames of {height}x{width}x{channels} are too large")
        }
    })
}

/// A zeroed buffer of `len` bytes, raising `too_large()` when the size overflowed (`None`) or
/// exceeds `isize::MAX`, and `MemoryError` when it can't be allocated.
fn zeroed(len: Option<usize>, too_large: impl FnOnce() -> String) -> errors::Result<Vec<u8>> {
    let len = len
        .filter(|&len| isize::try_from(len).is_ok())
        .ok_or_else(|| PyValueError::new_err(too_large()))?;
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(len)
        .map_err(|error| PyMemoryError::new_err(error.to_string()))?;
    buffer.resize(len, 0);
    Ok(buffer)
}

/// Moves a filled frame buffer into a numpy `uint8[height, width, channels]` array without
/// copying. The array owns the buffer from then on (docs/native.md, "Arrays").
pub(crate) fn into_frame_array(
    py: Python<'_>,
    buffer: Vec<u8>,
    height: usize,
    width: usize,
    channels: usize,
) -> Bound<'_, PyArray3<u8>> {
    Array3::from_shape_vec((height, width, channels), buffer)
        .expect("frame_buffer sized the buffer for this shape")
        .into_pyarray(py)
}

/// Moves a filled buffer of `frames` frames into a numpy
/// `uint8[frames, height, width, channels]` array without copying, like
/// [`into_frame_array`].
pub(crate) fn into_frames_array(
    py: Python<'_>,
    buffer: Vec<u8>,
    frames: usize,
    height: usize,
    width: usize,
    channels: usize,
) -> Bound<'_, PyArray4<u8>> {
    Array4::from_shape_vec((frames, height, width, channels), buffer)
        .expect("frames_buffer sized the buffer for this shape")
        .into_pyarray(py)
}

/// A `uint8[height, width, 3]` frame of `gth_frame::fill_test_pattern`.
#[pyfunction]
fn fill_test_pattern(
    py: Python<'_>,
    height: usize,
    width: usize,
    seed: u8,
) -> errors::Result<Bound<'_, PyArray3<u8>>> {
    // Zeroing and filling are O(pixels), so both run detached. A PyErr needs no GIL to create.
    let buffer = py.detach(|| {
        let mut buffer = frame_buffer(height, width, RGB8_BYTES_PER_PIXEL)?;
        gth_frame::fill_test_pattern(&mut buffer, height, width, seed);
        Ok::<_, errors::Error>(buffer)
    })?;
    Ok(into_frame_array(
        py,
        buffer,
        height,
        width,
        RGB8_BYTES_PER_PIXEL,
    ))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(fill_test_pattern, module)?)?;
    Ok(())
}
