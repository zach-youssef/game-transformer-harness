# Architecture

The harness runs libretro emulator cores headless behind a Python
[gymnasium](https://gymnasium.farama.org/) environment. Everything that happens once per
emulated frame is Rust; Python is called once per agent step and only orchestrates.

## Crates

| Crate | Holds | Doesn't hold |
|---|---|---|
| `gth-core` | The libretro host: load a core dynamic library, answer its callbacks, run frames (including frame skip), savestates, RAM and memory-map reads. | Python, windowing, audio output |
| `gth-frame` | Per-frame pixel ops: pixel format to RGB8, crop, tate rotation, resize to the model size, all into caller-provided buffers. | Anything core-specific |
| `gth-record` | The recording format: replay log (start state and per-frame inputs), desync checkpoints, training-shard export. | Frame storage (frames are re-rendered from the replay) |
| `gth-config` | Config types (serde + TOML): `game.toml`, `cores.lock`, the action space, RAM variables. | Emulation and frame data |
| `gth-py` | The PyO3 extension module `gtharness._native`: glue that converts types and errors. | Logic: it goes in the crate it belongs to |
| `gth-libretro-sys` | The FFI subset of `libretro.h` we use, hand-written (docs/libretro.md). | Logic, safe wrappers (those are `gth-core`'s) |
| `gth-testcore` | A tiny deterministic libretro core (a `cdylib`) that tests and CI load instead of real cores and ROMs. | Dependencies on the host crates (it may use `gth-libretro-sys`) |
| `gth-test-support` | Helpers shared by Rust tests (a dev-dependency): building and locating the test core. | Anything non-test code uses |

The Python package `gtharness` (in `python/gtharness/`) holds the environment API, the CLI and
the glue to policies and trainers.

Planned crates, added by the task that first needs them:

- `gth-play`: the human-play binary (window, audio, gamepad). Its windowing dependencies stay
  out of the workspace until then (P6).

## Data flow

```
Python (gtharness: env, CLI)
  -> gtharness._native        (gth-py: glue)
    -> gth-core               (host)  + gth-frame, gth-config, gth-record
      -> libretro core        (.so / .dylib / .dll, loaded at runtime)
```

One agent step is one call into `_native`: it holds the input for `frameskip` frames inside
`gth-core`, converts and resizes only the last frame with `gth-frame`, and returns it to Python
as an array.

## Frame lifecycle

One `RetroEnv.step(action)` (`python/gtharness/env.py`) is one call into `_native.Core.step`
(`crates/gth-py/src/core.rs`), which releases the GIL and:

1. `gth_core::Core::run_frames(frameskip, buttons)` calls `retro_run` `frameskip` times with
   the same buttons held. This is where the frame skip happens: the intermediate frames never
   leave Rust. `frameskip` is an argument of every call, so `RetroEnv.frameskip` can change
   between steps.
2. During each `retro_run`, the core's video callback copies its frame into the host's frame
   buffer (packed rows, in the core's pixel format), reusing the allocation. A duplicate frame
   (`NULL` data, `GET_CAN_DUPE`) keeps the previous one. Audio is counted and dropped.
3. After the last frame, `Core::with_frame` lends that buffer, under the host lock, to
   `gth_frame::Pipeline::render`, which crops, converts to RGB8 and rotates in one pass, then
   resizes (`fast_image_resize`, area or bilinear) into a newly allocated observation buffer.
   The pipeline keeps its scratch buffer and resizer, so the resize allocates nothing.
4. `gth-py` moves the buffer into a numpy array without copying (native.md, "Arrays").

`RetroEnv.reset()` restores the state right after loading (or loads `options["state"]`),
then runs `frameskip` frames with no button held and returns the last one, through the same
`step` path. A core draws only during `retro_run` and a savestate holds no frame, so without
those frames the first observation would be black; they are deterministic, so every episode
from the same start is identical, and `info["episode_frame"]` counts them. The host also
clears its frame after load, reset and unserialize, so an observation never shows a frame
from before them.

## One core per process

A process loads at most one libretro core, and a second load is an error:

- libretro callbacks carry no user pointer, so the host has to keep the running core in a
  process-global.
- Cores keep their own global state, so two instances of a core in one process would share it.
- Parallel environments are separate processes.

How `gth-core` keeps that safe (`crates/gth-core/src/host.rs`, `core.rs`):

- `Core::load` takes a claim (an atomic flag) before touching the global, and fails with
  `CoreLoad` if another `Core` holds it. Dropping the `Core` unloads the game, deinits,
  removes its temporary save directory and closes the library, then clears the global and
  releases the claim, so a later load starts fresh.
- The global host state (last frame, held buttons, core options, memory map, audio count)
  sits in a `Mutex`. Each callback locks it for its own duration and never panics (a panic in
  an `extern "C"` function aborts). `Core` never holds the lock while it calls into the core,
  so callbacks can't deadlock against it.
- Every method that calls into the core takes `&mut self`, so the borrow checker enforces
  libretro's one-call-at-a-time rule. A `Core` may move between threads between calls.
- Slices of core memory (`memory`, `read_mapped`) borrow the `Core`, so no frame can run, and
  change them, while one is alive.

## Repository layout

```
crates/            Rust workspace members (gth-*)
python/gtharness/  Python package; maturin puts _native next to it
tests/             Python tests (pytest); Rust tests live in their crates
docs/              focused docs, indexed from CLAUDE.md
data/              user-supplied ROMs and BIOS, git-ignored (docs/roms.md)
```

## Native code

Hot paths (callbacks, pixel conversion, resize, the frame-skip loop, recording, replay
checks, ROM hashing, RAM search) are Rust. The only C or C++ involved is third-party: the
cores we load and libraries bundled by crates (for example libzstd).
