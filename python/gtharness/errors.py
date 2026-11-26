"""The harness's exceptions, defined in Rust (crates/gth-py); all derive from HarnessError."""

from gtharness._native import CoreLoadError, DesyncError, HarnessError

__all__ = ["CoreLoadError", "DesyncError", "HarnessError"]
