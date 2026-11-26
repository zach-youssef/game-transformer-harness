"""Headless libretro cores behind a gymnasium environment, with the per-frame work in Rust."""

from gtharness import _native  # fails early if the extension isn't built

__version__ = _native.__version__
