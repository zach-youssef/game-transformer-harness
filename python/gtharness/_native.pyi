"""Type stubs for the extension module built from crates/gth-py (docs/native.md).

tests/test_native_stubs.py fails when these drift from the module.
"""

__version__: str

def build_info() -> dict[str, str]: ...
