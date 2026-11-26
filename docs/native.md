# Native bindings

`gtharness._native` is the extension module built from `crates/gth-py` with PyO3 and maturin.
Read this before adding or changing anything it exports.

## Rules

- All logic lives in the crate it belongs to (architecture.md). `gth-py` is glue: it converts
  arguments, calls one crate function, and converts the result or error. A branch that decides
  anything beyond that belongs in a crate, where Rust tests cover it.
- Every export is in `python/gtharness/_native.pyi`. `tests/test_native_stubs.py` fails when a
  name or a function's parameter names differ between the stub and the module.
- Python code imports what `_native` exports from the package modules that re-export it, not
  from `_native` directly, except for the environment's hot path.

## Adding a function

1. Write and test it in its crate, in plain Rust types (`&mut [u8]`, integers, crate errors).
2. In `gth-py`, add a `#[pyfunction]` that converts and calls it, following the conventions
   below, and register it in the `#[pymodule]` function.
3. Add it to `_native.pyi`, then test it from Python in `tests/`.

## Build info

`_native.__version__` is the workspace version (`Cargo.toml`), which `pyproject.toml` repeats.
`_native.build_info()` returns the version, git commit, target triple, cargo profile and rustc
version, recorded in every dataset's metadata, as a dict sorted by key so serialized metadata
is stable. `crates/gth-py/build.rs` captures them at build time and reruns when HEAD or the
Rust sources change; a build outside a git checkout records `unknown`.

`git_sha` is the commit, not uncommitted changes. `git_dirty` is `"true"` when the files that
build the extension (`crates/`, `Cargo.toml`, `Cargo.lock`) differed from it at build time.
Python sources aren't covered, so record datasets from a clean checkout.

## Why Rust and PyO3

- Per-frame work (callbacks, pixel conversion, the frame-skip loop, recording) must not pay
  Python's per-call cost or hold the GIL, and Rust does it without a C or C++ toolchain.
- PyO3 with the `abi3-py312` feature builds one module per OS for every CPython from 3.12 on,
  and maturin builds and installs it in one command (development.md).
- The `numpy` crate hands frames to Python as arrays without copying.
