"""Frames reach Python as numpy arrays that own the Rust buffer (no copy)."""

import numpy as np
import pytest

from gtharness import _native


def expected_pattern(height: int, width: int, seed: int) -> np.ndarray:
    y, x = np.indices((height, width))
    channels = [(x + seed) % 256, (y + seed) % 256, (x ^ y ^ seed) % 256]
    return np.stack(channels, axis=-1).astype(np.uint8)


@pytest.mark.parametrize(("height", "width", "seed"), [(224, 256, 0), (3, 300, 255), (1, 1, 7)])
def test_shape_dtype_and_contents(height, width, seed):
    frame = _native.fill_test_pattern(height, width, seed)
    assert frame.shape == (height, width, 3)
    assert frame.dtype == np.uint8
    np.testing.assert_array_equal(frame, expected_pattern(height, width, seed))


def test_array_owns_the_rust_buffer():
    frame = _native.fill_test_pattern(4, 5, 1)
    # A copy into numpy-allocated memory would own its data; the moved Rust buffer is held by
    # the array's base object instead.
    assert not frame.flags.owndata
    assert frame.base is not None and not isinstance(frame.base, np.ndarray)
    assert frame.flags.c_contiguous
    assert frame.flags.writeable
    frame[0, 0] = 99  # the caller's to keep and change
    assert (frame[0, 0] == 99).all()


def test_empty_frames():
    assert _native.fill_test_pattern(0, 5, 0).shape == (0, 5, 3)
    assert _native.fill_test_pattern(5, 0, 0).shape == (5, 0, 3)


def test_bad_arguments():
    with pytest.raises(OverflowError):
        _native.fill_test_pattern(-1, 4, 0)
    with pytest.raises(OverflowError):
        _native.fill_test_pattern(4, 4, 256)
    with pytest.raises(ValueError, match="too large"):
        _native.fill_test_pattern(2**62, 2**62, 0)  # overflows usize
    with pytest.raises(ValueError, match="too large"):
        _native.fill_test_pattern(2**62, 1, 0)  # fits usize, not isize (Rust's allocation limit)
    with pytest.raises(MemoryError):
        _native.fill_test_pattern(2**40, 2**20, 0)  # 3 EiB: no allocator can provide it
