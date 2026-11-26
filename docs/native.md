# Native bindings

`gtharness._native` is the extension module built from `crates/gth-py` with PyO3 and maturin.
Read this before adding or changing anything it exports.

## Rules

- All logic lives in the crate it belongs to (architecture.md). `gth-py` is glue: it converts
  arguments, calls one crate function, and converts the result or error. A branch that decides
  anything beyond that belongs in a crate, where Rust tests cover it.
- Every export is in `python/gtharness/_native.pyi`. `tests/test_native_stubs.py` fails when a
  name or a function's parameter names differ between the stub and the module.
- Python code imports what `_native` exports from the package modules that re-export it (such
  as `gtharness.errors`), not from `_native` directly, except for the environment's hot path.
- Test hooks, which let pytest drive a convention before a real binding uses it, have
  underscore names and live in `gth-py/src/testing.rs`. Only `tests/` calls them.

## Adding a function

1. Write and test it in its crate, in plain Rust types (`&mut [u8]`, integers, crate errors).
2. In `gth-py`, add a `#[pyfunction]` that converts and calls it, following the conventions
   below, and register it in the `#[pymodule]` function.
3. Add it to `_native.pyi`, then test it from Python in `tests/`.

## Errors

- Each crate has one `thiserror` error enum (`gth_core::Error`), with messages that name the
  cause. Panics are for bugs only; PyO3 turns a panic into `pyo3_runtime.PanicException`.
- A `#[pyfunction]` that can fail returns `errors::Result<T>` (`gth-py/src/errors.rs`). `?`
  converts both crate errors and `PyErr` into it, and PyO3 raises it as the mapped exception.
- The Python exceptions are defined in Rust with `create_exception!` and re-exported by
  `gtharness.errors`. Every one derives from `HarnessError`:

  | Crate error | Python exception |
  |---|---|
  | `gth_core::Error::CoreLoad` | `CoreLoadError` |
  | `gth_core::Error::Desync` | `DesyncError` |

- Their `__module__` is `gtharness.errors`, so they pickle across the process boundary of
  parallel environments.
- The crate enums aren't `#[non_exhaustive]` and the mapping has no wildcard arm, so a new
  variant fails to compile until it is mapped. Map it to a new `HarnessError` subclass when
  Python code would catch it separately, otherwise to `HarnessError` itself.
- Bad arguments from Python raise the built-in exceptions (`ValueError`, `TypeError`), and
  argument conversion already raises `OverflowError` (negative or too large integers) and
  `TypeError`.

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
