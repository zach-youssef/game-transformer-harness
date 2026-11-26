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
| `gth-testcore` | A tiny deterministic libretro core (a `cdylib`) that tests and CI load instead of real cores and ROMs. | Dependencies on the host crates (it may use `gth-libretro-sys`) |

The Python package `gtharness` (in `python/gtharness/`) holds the environment API, the CLI and
the glue to policies and trainers.

Planned crates, added by the task that first needs them:

- `gth-libretro-sys`: the FFI subset of `libretro.h` we use (stack PR S3).
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

## One core per process

A process loads at most one libretro core, and a second load is an error:

- libretro callbacks carry no user pointer, so the host has to keep the running core in a
  process-global.
- Cores keep their own global state, so two instances of a core in one process would share it.
- Parallel environments are separate processes.

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
