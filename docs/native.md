# Native bindings

`gtharness._native` is the extension module built from `crates/gth-py` with PyO3 and maturin.
Read this before adding or changing anything it exports.

## Rules

- All logic lives in the crate it belongs to (architecture.md). `gth-py` is glue: it converts
  arguments, calls one crate function, and converts the result or error. A branch that decides
  anything beyond that belongs in a crate, where Rust tests cover it.
- Every export is in `python/gtharness/_native.pyi`. `tests/test_native_stubs.py` fails when a
  name or a function's parameter names differ between the stub and the module.
- Python code imports the exceptions from `gtharness.errors` and `Replayer` from
  `gtharness.record`, the modules that re-export them, never from `_native`. Classes and
  functions with no re-export (`Core`, `GameConfig`, `RamSearch`, `rom_dir`,
  `user_config_path`, `export_shard`, `decode_frames`, `decode_sprites`) are used from `_native` directly, by
  the package's own modules (`env.py`, `cli.py`, `ramsearch.py`, `shards.py`) and tests.
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
  | `gth_core::Error::RamSearch` | `HarnessError` |
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

`decode_frames` returns a whole frame chunk as one `uint8[n, height, width, 3]` array the
same way (`rgb8_frames_buffer`, `into_rgb8_frames`).

The lifetime rule: an array `_native` returns owns its memory. Rust never writes to or frees
a buffer after handing it over, so callers may keep, change and share frames across threads,
for example in a replay buffer. `_native` never returns a view of memory that Rust keeps and
reuses (`PyArray::borrow_from_array`): a later step would silently change frames the caller
still holds. The cost is one allocation per returned frame, small next to emulating it.

## Core

`_native.Core` (`gth-py/src/core.rs`) owns a `gth_core::Core` and the `gth_frame::Pipeline`
for its observations. `gtharness.env.RetroEnv` is its only user; it is the environment's hot
path, so the env imports it from `_native` directly.

- The constructor takes the core options, the settle frames after savestate loads
  (docs/games.md, `settle_frames`) and every pipeline choice explicitly (observation size,
  crop, rotation, interpolation); `RetroEnv` takes the options, settle frames, crop and
  rotation from the game config, requires the size and defaults only `interpolation` ("area").
- `reset()` returns to the state right after loading (gth-core's load snapshot, or
  `retro_reset` for cores that can't serialize then; docs/libretro.md, "Host").
  `resets_to_load_state` (also on `RetroEnv`) says which one the core gets.
- `step(frames, buttons, sticky_prob)` runs the frames, with sticky actions at
  `sticky_prob` (0 for none), and returns the last one as a new observation array, all with
  the GIL released (architecture.md, "Frame lifecycle"). `seed(seed)` seeds the core's RNG
  (`gth_core::Rng`; 0 until seeded), which sticky actions and `random_below(n)` (a uniform
  integer in `0..n`, for no-op starts) draw from; it runs on across resets and loads.
- `read_ram(config)` decodes the game's RAM variables into a `dict[str, int]` and evaluates
  the episode condition on them, returning `(ram, terminated)`: one call per step for both.
  `check_ram(config)` raises if a variable isn't in the core's memory (`RetroEnv` calls it
  once after loading). `run_inputs(inputs)` plays a reset script.
- `close()` drops the `gth_core::Core`, which unloads it and frees the process's one core slot
  (architecture.md, "One core per process"); later calls raise `ValueError`. Dropping the
  Python object does the same, but only when the garbage collector gets to it, so code that
  loads another core closes the first explicitly.
- `start_recording(directory, config, game_id, core_lock, recorder, player, license,
  checks_every)` and `finish_recording()` wrap a `gth_record::Recorder`
  (docs/recording-format.md). `core_lock` is a `CoresLock` whose entry for the game's core
  goes into `meta.toml`, or None; `RetroEnv` passes the checkout's lock only when it pins the
  loaded core file (`gtharness.cores.pinned_lock`). While a recording is
  in progress, `step` and `run_inputs` run through it (each pair is one recorded step), and
  `reset` and `unserialize` raise `ValueError`, since a state load would break the replay.
  `close()` leaves an unfinished recording incomplete. A failed write raises from the call
  that ran into it and drops the recorder, leaving that recording incomplete, so the next
  `start_recording` works.
- Methods take `&mut self`, so PyO3 raises `RuntimeError` (a borrow error) if two Python
  threads call one `Core` at once rather than calling into the core concurrently.

## Replayer

`_native.Replayer` (`gth-py/src/record.rs`, re-exported as `gtharness.record.Replayer`) owns a
`gth_record::Replayer`, which owns its core, and an observation pipeline taking the same
explicit arguments as `Core`'s. `step()` replays one agent step (GIL released) and returns
`(joypad mask of its first frame, frames, observation)`, or `None` at the end; `run_to_end()`
verifies the rest without rendering. A desync raises `DesyncError`. Like `Core`, it holds the
process's one core slot until `close()`.

## Shards

`_native.export_shard` (`gth-py/src/shard.rs`) opens a recording, loads its core, runs
`gth_record::export` with the GIL released and returns the counts as a dict; the core is
unloaded when it returns. Its arguments are the `Replayer`'s plus the output path, the
frameskip (`None` for the recording's steps), the filter's name, the chunk size and
`sprites`; nothing has a default. `_native.decode_frames(data, samples, height, width, path)`
decodes one frame chunk and `_native.decode_sprites(data, records, path)` one sprite chunk,
as `bytes` that `gtharness.shards` views with `SPRITE_DTYPE` (GIL released). `gtharness.shards`
calls them: one spawned process per recording for
the export, and `Shard`/`batches` for reading (docs/recording-format.md, "Training shards").

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

## RamSearch

`_native.RamSearch` (`gth-py/src/ramsearch.rs`) owns a `gth_core::RamSearch` (docs/games.md,
"RAM search") and no core: every method that reads memory takes the `Core` to search, which
must be the one it started on (the memory layout is checked). Spaces, views and relations
are passed as their names and parsed by `gth-config`, a bad name raising `ValueError`.
`filter`, `rebase`, `watch` and `run_script` release the GIL; `watch(core, inputs, limit)`
and `run_script(core, script_path)` run frames outside the recorder, so they raise
`ValueError` while the core records, like `reset`. `candidates(limit)` returns dicts (view,
location, size, encoding, swap16, value), and `ram_search_views()` lists the views `"all"`
expands to. `gtharness.ramsearch` wraps it for environments and is its only user besides
tests.

## Cores lock

`_native.CoresLock` (`gth-py/src/cores.rs`) is a parsed `cores.lock` (docs/cores.md),
immutable: the core `names`, each core's source (`core(name)`) and hashes
(`artifacts(name)`), the `release_tag`, `with_builds(dir, targets)` (a copy with the hashes of
the builds in `dir/<target>/`), and the checks `check_artifact(name, target, path)` and
`check_library_version(name, reported)`, which raise `ConfigError`, and `pins(name, target,
path)`, whether `path` is the pinned build (`gth_config::CoresLock::pinned`, which `gth-play`
uses too). `library_file` and `asset_name` name a core's files per target triple.
`probe_core(core_path, system_dir)` is `gth_core::Core::probe`: it loads a core without a game
and unloads it, so it raises `CoreLoadError` while a `Core` is open in the process.
`gtharness.cores` moves the files.

`tests/test_native_stubs.py` also checks classes: their public members and each method's
parameter names must match the stub, and special methods the stub lists (`__len__`) must
exist.

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
is stable. It wraps `gth_build_info::build_info()` (`crates/gth-build-info`), which `gth-play`
records too, so both describe a build the same way. That crate's `build.rs` captures them at
build time and reruns when HEAD or the Rust sources change; a build outside a git checkout
records `unknown`.

`git_sha` is the commit, not uncommitted changes. `git_dirty` is `"true"` when the files that
build the extension (`crates/`, `Cargo.toml`, `Cargo.lock`) differed from it at build time.
Python sources aren't covered, so record datasets from a clean checkout.

## Why Rust and PyO3

- Per-frame work (callbacks, pixel conversion, the frame-skip loop, recording) must not pay
  Python's per-call cost or hold the GIL, and Rust does it without a C or C++ toolchain.
- PyO3 with the `abi3-py312` feature builds one module per OS for every CPython from 3.12 on,
  and maturin builds and installs it in one command (development.md).
- The `numpy` crate hands frames to Python as arrays without copying.
