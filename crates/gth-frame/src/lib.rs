//! Per-frame pixel ops into caller buffers: format conversion, crop, rotate, resize.

/// Bytes per pixel of RGB8, the format frames reach Python in.
pub const RGB8_BYTES_PER_PIXEL: usize = 3;

/// Length in bytes of a `height` x `width` RGB8 frame, or `None` if it overflows `usize`.
pub fn rgb8_len(height: usize, width: usize) -> Option<usize> {
    height.checked_mul(width)?.checked_mul(RGB8_BYTES_PER_PIXEL)
}

/// Fills `dst`, a row-major `height` x `width` RGB8 frame, with a pattern that tests can
/// recompute: pixel (y, x) is `[x + seed, y + seed, x ^ y ^ seed]`, each modulo 256.
///
/// # Panics
///
/// If `dst.len()` isn't [`rgb8_len`]`(height, width)`: a bug in the caller.
pub fn fill_test_pattern(dst: &mut [u8], height: usize, width: usize, seed: u8) {
    assert_eq!(
        Some(dst.len()),
        rgb8_len(height, width),
        "dst must hold height x width RGB8 pixels"
    );
    let (pixels, _) = dst.as_chunks_mut::<RGB8_BYTES_PER_PIXEL>();
    for (i, pixel) in pixels.iter_mut().enumerate() {
        // Truncating to u8 is the modulo 256.
        let (y, x) = ((i / width) as u8, (i % width) as u8);
        *pixel = [x.wrapping_add(seed), y.wrapping_add(seed), x ^ y ^ seed];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb8_len_checks_overflow() {
        assert_eq!(rgb8_len(224, 256), Some(224 * 256 * 3));
        assert_eq!(rgb8_len(0, 5), Some(0));
        assert_eq!(rgb8_len(usize::MAX, 1), None);
        assert_eq!(rgb8_len(usize::MAX / 2, 2), None);
    }

    #[test]
    fn test_pattern_pixels() {
        let (height, width) = (3, 300);
        let mut frame = vec![0; rgb8_len(height, width).unwrap()];
        fill_test_pattern(&mut frame, height, width, 10);
        let pixel = |y: usize, x: usize| &frame[(y * width + x) * 3..][..3];
        assert_eq!(pixel(0, 0), [10, 10, 10]);
        assert_eq!(pixel(2, 1), [11, 12, 3 ^ 10]);
        assert_eq!(pixel(1, 299), [(299 + 10) as u8, 11, (299 ^ 1) as u8 ^ 10]);
    }

    #[test]
    fn empty_frames() {
        fill_test_pattern(&mut [], 0, 4, 1);
        fill_test_pattern(&mut [], 4, 0, 1);
    }

    #[test]
    #[should_panic(expected = "dst must hold")]
    fn wrong_buffer_size_panics() {
        fill_test_pattern(&mut [0; 6], 1, 1, 0);
    }
}
