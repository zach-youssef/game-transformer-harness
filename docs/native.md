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

- Each crate has one `thiserror` error enum (`gth_core::Error`, `gth_config::Error`,
  `gth_record::Error`), with messages that name the cause. Panics are for bugs only; PyO3
  turns a panic into `pyo3_runtime.PanicException`.
- A `#[pyfunction]` that can fail returns `errors::Result<T>` (`gth-py/src/errors.rs`). `?`
  converts both crate errors and `PyErr` into it, and PyO3 raises it as the mapped exception.
- The Python exceptions are defined in Rust with `create_exception!` and re-exported by
  `gtharness.errors`. Every one derives from `HarnessError`:

  | Crate error | Python exception |
  |---|---|
  | `gth_core::Error::CoreLoad` | `CoreLoadError` |
  | `gth_core::Error::Content` (content file unreadable) | `ConfigError` |
  | `gth_core::Error::Savestate` | `HarnessError` |
  | `gth_core::Error::Ram` | `HarnessError` |
  | `gth_frame::Error` (any) | `HarnessError` |
  | `gth_config::Error` (any) | `ConfigError` |
  | `gth_record::Error::Desync` | `DesyncError` |
  | `gth_record::Error::Mismatch` (another core build or content) | `ConfigError` |
  | `gth_record::Error::Io`, `Format`, `Failed` | `HarnessError` |
  | `gth_record::Error::Core`, `Config` | as the wrapped error |

- Their `__module__` is `gtharness.errors`, so they pickle across the process boundary of
  parallel environments.
- The crate enums aren't `#[non_exhaustive]` and the mapping has no wildcard arm, so a new
  variant fails to compile until it is mapped. Map it to a new `HarnessError` subclass when
  Python code would catch it separately, otherwise to `HarnessError` itself.
- Bad arguments from Python raise the built-in exceptions (`ValueError`, `TypeError`), and
  argument conversion already raises `OverflowError` (negative or too large integers) and
  `TypeError`.

## Arrays

Frames reach Python as numpy `uint8[height, width, 3]` arrays (RGB8, row-major), without a
copy (`gth-py/src/frame.rs`):

1. `gth-py` allocates the buffer (`rgb8_buffer`). Sizes from Python are untrusted, so a size
   that overflows raises `ValueError` and one that can't be allocated raises `MemoryError`
   instead of aborting the process.
2. A crate function fills it as a plain `&mut [u8]`, as `gth_frame::fill_test_pattern` does.
3. `into_rgb8_array` moves the `Vec` into the array (`IntoPyArray`): numpy views the Rust
   allocation, and the array's base object frees it when the last array or view of it goes.

The lifetime rule: an array `_native` returns owns its memory. Rust never writes to or frees
a buffer after handing it over, so callers may keep, change and share frames across threads,
for example in a replay buffer. `_native` never returns a view of memory that Rust keeps and
reuses (`PyArray::borrow_from_array`): a later step would silently change frames the caller
still holds. The cost is one allocation per returned frame, small next to emulating it.

## Core

`_native.Core` (`gth-py/src/core.rs`) owns a `gth_core::Core` and the `gth_frame::Pipeline`
for its observations. `gtharness.env.RetroEnv` is its only user; it is the environment's hot
path, so the env imports it from `_native` directly.

- The constructor takes the core options and every pipeline choice explicitly (observation
  size, crop, rotation, interpolation); `RetroEnv` takes the options, crop and rotation from
  the game config, requires the size and defaults only `interpolation` ("area").
- `reset()` returns to the state right after loading (gth-core's load snapshot, or
  `retro_reset` for cores that can't serialize then; docs/libretro.md, "Host").
  `resets_to_load_state` (also on `RetroEnv`) says which one the core gets.
- `step(frames, buttons)` runs the frames and returns the last one as a new observation array,
  all with the GIL released (architecture.md, "Frame lifecycle").
- `read_ram(config)` decodes the game's RAM variables into a `dict[str, int]` and evaluates
  the episode condition on them, returning `(ram, terminated)`: one call per step for both.
  `check_ram(config)` raises if a variable isn't in the core's memory (`RetroEnv` calls it
  once after loading). `run_inputs(inputs)` plays a reset script.
- `close()` drops the `gth_core::Core`, which unloads it and frees the process's one core slot
  (architecture.md, "One core per process"); later calls raise `ValueError`. Dropping the
  Python object does the same, but only when the garbage collector gets to it, so code that
  loads another core closes the first explicitly.
- `start_recording(directory, config, game_id, recorder, player, license, checks_every)` and
  `finish_recording()` wrap a `gth_record::Recorder` (docs/recording-format.md): while one is
  in progress, `step` and `run_inputs` run through it (each pair is one recorded step), and
  `reset` and `unserialize` raise `ValueError`, since a state load would break the replay.
  `close()` leaves an unfinished recording incomplete. A failed write raises from the call
  that ran into it and drops the recorder, leaving that recording incomplete, so the next
  `start_recording` works.
- Methods take `&mut self`, so PyO3 raises `RuntimeError` (a borrow error) if two Python
  threads call one `Core` at once rather than calling into the core concurrently.

## Replayer

`_native.Replayer` (`gth-py/src/record.rs`, re-exported as `gtharness.record.Replayer`) owns
a `gth_record::Replayer`, which owns its core, and an observation pipeline taking the same
explicit arguments as `Core`'s. `step()` replays one agent step (GIL released) and returns
`(joypad mask, frames, observation)`, or `None` at the end; `run_to_end()` verifies the rest without
rendering. A desync raises `DesyncError`. Like `Core`, it holds the process's one core slot
until `close()`.

## GameConfig

`_native.GameConfig` (`gth-py/src/config.rs`) is a parsed `game.toml` (docs/games.md) and its
action space (docs/actions.md), immutable (`frozen`). It exposes what `RetroEnv` needs as
getters and methods: the frame and core settings, the reset and `check_reset_state(bytes)`
(the savestate's SHA-1), `slots` (always the four) and `mapped_slots`, `encode`/`decode`
between `(direction, buttons)` and flat actions, `joypad_table()` (the RetroPad mask of
every flat action, so the env maps an action with one list lookup), `terminated(ram)`, and
the file checks `find_content(rom_dir)`, `missing_parents(content)` and
`missing_bios(system_dir)`. `rom_dir()` and
`user_config_path()` are module functions (docs/roms.md).

`tests/test_native_stubs.py` also checks classes: their public members and each method's
parameter names must match the stub.

## GIL

- Release the GIL around every native call that does real work (a frame step, a pixel
  conversion, file or network I/O) with `py.detach(|| ...)`, the PyO3 0.26+ name for
  `allow_threads`. Other Python threads (a data loader, a second environment thread, a
  logger) then run during the call, and native calls on different threads run in parallel.
- Convert arguments before `detach` and results after it: the closure can't touch Python
  objects, which PyO3 enforces by requiring it to be `Send`. `fill_test_pattern` allocates
  and fills the buffer detached, then builds the array.
- Calls that only convert a few values (`build_info`) keep the GIL: releasing it costs more
  than they do.
- Detaching doesn't make a resource thread-safe: whatever the closure shares across threads
  is locked by its owner in Rust (the core host, for example), not by the GIL.
- `tests/test_native_gil.py` checks that two threads blocked in `_native` overlap.

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
