//! Frames to numpy arrays: Rust fills a buffer, then moves it into the array without copying.

use numpy::ndarray::{Array3, Array4};
use numpy::{IntoPyArray, PyArray3, PyArray4};
use pyo3::exceptions::{PyMemoryError, PyValueError};
use pyo3::prelude::*;

use crate::errors;

/// A zeroed buffer for a `height` x `width` RGB8 frame. Python's size arguments are untrusted,
/// so a size that overflows or can't be allocated raises instead of aborting the process.
pub(crate) fn rgb8_buffer(height: usize, width: usize) -> errors::Result<Vec<u8>> {
    zeroed(gth_frame::rgb8_len(height, width), || {
        format!("frame {height}x{width} is too large")
    })
}

/// A zeroed buffer for `frames` RGB8 frames of `height` x `width`, checked like
/// [`rgb8_buffer`].
pub(crate) fn rgb8_frames_buffer(
    frames: usize,
    height: usize,
    width: usize,
) -> errors::Result<Vec<u8>> {
    let len = gth_frame::rgb8_len(height, width).and_then(|len| len.checked_mul(frames));
    zeroed(len, || {
        format!("{frames} frames of {height}x{width} are too large")
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

/// Moves a filled RGB8 buffer into a numpy `uint8[height, width, 3]` array without copying.
/// The array owns the buffer from then on (docs/native.md, "Arrays").
pub(crate) fn into_rgb8_array(
    py: Python<'_>,
    buffer: Vec<u8>,
    height: usize,
    width: usize,
) -> Bound<'_, PyArray3<u8>> {
    let shape = (height, width, gth_frame::RGB8_BYTES_PER_PIXEL);
    Array3::from_shape_vec(shape, buffer)
        .expect("rgb8_buffer sized the buffer for this shape")
        .into_pyarray(py)
}

/// Moves a filled buffer of `frames` RGB8 frames into a numpy `uint8[frames, height, width, 3]`
/// array without copying, like [`into_rgb8_array`].
pub(crate) fn into_rgb8_frames(
    py: Python<'_>,
    buffer: Vec<u8>,
    frames: usize,
    height: usize,
    width: usize,
) -> Bound<'_, PyArray4<u8>> {
    let shape = (frames, height, width, gth_frame::RGB8_BYTES_PER_PIXEL);
    Array4::from_shape_vec(shape, buffer)
        .expect("rgb8_frames_buffer sized the buffer for this shape")
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
        let mut buffer = rgb8_buffer(height, width)?;
        gth_frame::fill_test_pattern(&mut buffer, height, width, seed);
        Ok::<_, errors::Error>(buffer)
    })?;
    Ok(into_rgb8_array(py, buffer, height, width))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(fill_test_pattern, module)?)?;
    Ok(())
}
