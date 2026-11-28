# Architecture

The harness runs libretro emulator cores headless behind a Python
[gymnasium](https://gymnasium.farama.org/) environment. Everything that happens once per
emulated frame is Rust; Python runs once per agent step (the env's `step`, which makes two
native calls, and the reward function) and only orchestrates.

## Crates

| Crate | Holds | Doesn't hold |
|---|---|---|
| `gth-core` | The libretro host: load a core dynamic library, answer its callbacks, run frames (including frame skip), savestates, RAM and memory-map reads, RAM search (`ramsearch.rs`). | Python, windowing, audio output |
| `gth-frame` | Per-frame pixel ops: pixel format to RGB8, crop, tate rotation, max of two frames, BT.601 gray, resize to the model size, all into caller-provided buffers. | Anything core-specific |
| `gth-record` | The recording format (docs/recording-format.md): `Recorder` writes the start state, per-frame joypad masks and desync checks; `Recording` and `Replayer` read and re-run them; `export` renders a recording into a training shard (`shard.rs`), with its sprite lists from `gth-sprites` when asked. | Frame storage in recordings (frames are re-rendered by replaying) |
| `gth-sprites` | Sprite-list labels (docs/recording-format.md, "Sprite labels"): `SpriteStream` binds a board's decoder (`sprite_hw`) to a core's named state areas (docs/libretro.md, "Named areas") and reads the list the next frame draws, one `Sprite` record each. | Observations: labels are training targets only |
| `gth-config` | Config types (serde + TOML): `game.toml`, `cores.lock`, the action space, RAM variables, RAM search scripts and their views and relations; finding and auditing the user's ROM and BIOS files by hash. | Emulation and frame data |
| `gth-build-info` | `build_info()`, the harness build recorded in datasets' metadata (version, git commit and dirty flag, target, profile, rustc), captured by its build script: one function behind `_native.build_info()` and `gth-play`'s recordings (docs/native.md, "Build info"). | Anything else: its build script reruns whenever a crate changes |
| `gth-py` | The PyO3 extension module `gtharness._native`: glue that converts types and errors. | Logic: it goes in the crate it belongs to |
| `gth-libretro-sys` | The FFI subset of `libretro.h` we use, hand-written (docs/libretro.md). | Logic, safe wrappers (those are `gth-core`'s) |
| `gth-play` | The human-play recorder (docs/human-play.md): a library with the headless half (session and hotkeys, bindings, stick mapping, display scaling, audio rate control, latency probe, headless smoke) and the `gth-play` binary that adds the window (winit + softbuffer), audio (cpal) and gamepads (gilrs). The only crate with windowing, audio or controller dependencies. | Anything `gth-py` or Python uses; emulation (that's `gth-core`'s) |
| `gth-testcore` | A tiny deterministic libretro core (a `cdylib`) that tests and CI load instead of real cores and ROMs. | Dependencies on the host crates (it may use `gth-libretro-sys`) |
| `gth-test-support` | Helpers shared by Rust tests (a dev-dependency): building and locating the test core. | Anything non-test code uses |

The Python package `gtharness` (in `python/gtharness/`) orchestrates:

| Module | Holds |
|---|---|
| `env.py` | `RetroEnv`, the gymnasium environment, and `RetroEnv.from_game` (below) |
| `rewards.py` | Reward functions on RAM variables (`score_delta`), and `REQUIRED_RAM`, the variables each built-in one reads, checked when an env is made |
| `record.py` | `Record` (where and how `RetroEnv` records) and `Replayer` (docs/recording-format.md) |
| `shards.py` | Training shards (docs/recording-format.md): `export_shards` (one process per recording) and `gth export-shards`; `Shard` and `batches`, the PyTorch-free reader, whose batches hold no RAM and no sprite lists |
| `ramsearch.py` | RAM search on an environment's core, `assert_ram` for game tests, and `gth ram-search` (docs/games.md, "RAM search"); a developer tool, never policy input |
| `capture.py` | `gth capture`: scripted captures, a game recorded from its reset with an input script (docs/recording-format.md, "Scripted captures") |
| `bench.py` | `gth bench`: speed, savestate and determinism benchmarks of real cores (docs/performance.md) |
| `protocol.py` | Evaluation protocols: `load`, `EvalProtocol` and its canonical text and SHA-1 (docs/evaluation.md) |
| `evaluate.py` | `gth evaluate`: a policy played under a protocol, one process per game and seed pair, and `check_results`, the results schema (docs/evaluation.md, "Running an evaluation") |
| `policies/` | The `Policy` protocol and `ObsRequirements` (`__init__.py`), and the random policy (`random.py`) |
| `cli.py` | The `gth` command (`gth audit`, docs/roms.md; `gth ram-search`; `gth export-shards`; `gth cores`; `gth bench`; `gth capture`; `gth evaluate`) |
| `errors.py` | The exceptions, re-exported from `_native` (docs/native.md, "Errors") |
| `_native.pyi` | Type stubs of the extension module (docs/native.md) |

Policy adapters beyond the random one come with the tasks that need them (NitroGen bridge
P9, training P10).

## Data flow

```
Python (gtharness: env, CLI)
  -> gtharness._native        (gth-py: glue)
    -> gth-core               (host)  + gth-frame, gth-config, gth-record
         gth-config           (game.toml, action space, ROM lookup and audit; also used by gth-py)
      -> libretro core        (.so / .dylib / .dll, loaded at runtime)
```

A recording (`gth-record`) borrows the env's core for each step while recording; a replay
owns its own core.

One agent step is two calls into `_native`: one holds the input for `frameskip` frames inside
`gth-core`, converts and resizes only the last frame with `gth-frame`, and returns it to Python
as an array; the other (`_native.Core.read_ram`) decodes the game's RAM variables for the
reward and evaluates the episode end on them, returning both.

## Frame lifecycle

The frame half of one `RetroEnv.step(action)` (`python/gtharness/env.py`) is one call into
`_native.Core.step` (`crates/gth-py/src/core.rs`), which releases the GIL and:

1. `gth_core::Core::run_sticky(frames, buttons, sticky_prob, rng)` calls `retro_run`
   `frames` times (`frameskip`, or fewer on the step that reaches the episode cap). On each
   frame, with probability `sticky_prob` the previous frame's buttons are held again instead
   of `buttons` (sticky actions, ALE's semantics; the frame before an episode's first holds
   nothing), drawing from the `gth_core::Rng` the `_native.Core` owns. At 0 it draws nothing
   and every frame holds `buttons`, as `Core::run_frames` does. This is where the frame skip
   happens: the intermediate frames never leave Rust. `frameskip` is an argument of every
   call, so `RetroEnv.frameskip` can change between steps. While recording, the recorder
   runs the frames and records each one's effective mask.
2. During each `retro_run`, the core's video callback copies its frame into the host's frame
   buffer (packed rows, in the core's pixel format), reusing the allocation. A duplicate frame
   (`NULL` data, `GET_CAN_DUPE`) keeps the previous one. Audio is counted and dropped (a
   player keeps it instead, `Core::set_audio_capture`). With max-pooling on
   (`Core::set_keep_previous_frame`), the host also keeps the frame of the run before: the
   first video call of each `retro_run` swaps the two buffers before writing, and a dupe or a
   run without a video call copies the frame (the screen shows it twice). Loads, resets and
   savestate loads black out both.
3. After the last frame, `Core::with_frame` (or `Core::with_frames`, with the previous frame)
   lends that buffer, under the host lock, to `gth_frame::Pipeline::render` (or
   `render_max`), which crops, converts to RGB8 and rotates in one pass, takes the
   per-channel maximum with the previous frame when pooling, converts to gray (integer
   BT.601 weights summing to 1, so r = g = b keeps its value) when the pipeline's `Color` is
   `Gray`, then resizes (`fast_image_resize`: area, bilinear, or bicubic as Pillow's
   `BICUBIC`) into a newly allocated observation buffer. `RetroEnv`'s required `maxpool`
   (settable) and `grayscale` choose these; at frameskip 1 the pooled frame is the
   previous step's last.
   The pipeline keeps its scratch buffers and resizer, so the resize allocates nothing. The
   area resize is the largest host-side cost of a step (about 0.35 ms per observation in a
   release build, measured during S4); profiling it is an open follow-up.
4. `gth-py` moves the buffer into a numpy array without copying (native.md, "Arrays").

Before the step, `RetroEnv` turns the action (docs/actions.md) into a RetroPad mask with one
lookup in the game's joypad table; after it, one `_native.Core.read_ram` call decodes the
game's RAM variables in Rust and evaluates the episode condition on them, and the reward
function (Python) gets the variables before and after the step. So a step is two native
calls plus the reward call.

`RetroEnv.reset(seed=...)` seeds the RNG when given a seed (without one it runs on from the
last episode), loads the game's reset savestate, or restores the state right after loading
and plays the game's reset script (`options["state"]` replaces both), runs the no-op start
(1..`noop_max` frames with no button held, drawn from the RNG; none at `noop_max = 0`), then
runs `frameskip` frames with no button held and returns the last one, through the same
`step` path. A core draws only during `retro_run` and a savestate holds no frame, so without
those frames the first observation would be black. The emulation is deterministic and all
the env's randomness comes from its seeded RNG, so a seed and the same actions replay an
episode exactly, and `info["episode_frame"]` counts these frames. The episode cap
(`max_episode_frames`) counts from the end of the script or state load, no-ops and the first
observation's frames included: the step that reaches it is shortened to end on it and
returns `truncated=True`. The host also
clears its frame after load, reset and unserialize, so an observation never shows a frame
from before them.

## Observations and RAM

The observation is the frame and nothing else. Game RAM is never part of it, so a policy
learns from pixels as a player does and transfers across games whose RAM differs. The game's
RAM variables (docs/games.md) feed only:

- the reward function, `reward(previous_ram, current_ram)` (`gtharness.rewards`): a required
  argument, so the choice of reward is always explicit (`score_delta` is the built-in);
- the episode end, the game's `episode.terminate` condition;
- `info["ram"]`, for logging and analysis. It must not be fed to a policy, and nothing in the
  harness does.

RAM search (`gtharness.ramsearch`, docs/games.md) is how a game's RAM variables are found
and checked; it is a developer tool and reads memory only for that.

`RetroEnv.from_game(game_id, data_dir, ...)` builds the environment from
`games/<game_id>/game.toml`: it finds the content by SHA-1 in the ROM directory and the rest
in `data_dir` (docs/roms.md, "Data directory").

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
- The global host state (last frame, held buttons, core options, memory map, audio count and
  captured samples) sits in a `Mutex`. Each callback locks it for its own duration and never
  panics (a panic in an `extern "C"` function aborts). `Core` never holds the lock while it calls into the core,
  so callbacks can't deadlock against it.
- Every method that calls into the core takes `&mut self`, so the borrow checker enforces
  libretro's one-call-at-a-time rule. A `Core` may move between threads between calls.
- Slices of core memory (`memory`, `read_mapped`) borrow the `Core`, so no frame can run, and
  change them, while one is alive.

## Repository layout

```
crates/            Rust workspace members (gth-*)
python/gtharness/  Python package; maturin builds _native into it
games/             one directory per game with its game.toml (docs/games.md)
tests/             Python tests (pytest); Rust tests live in their crates
scripts/           build-core.sh, the cores workflow's build recipe (docs/cores.md)
cores.lock         the pinned core builds (docs/cores.md)
docs/              focused docs, indexed from CLAUDE.md
data/              user-supplied ROMs and BIOS, git-ignored (docs/roms.md)
```

## Native code

Hot paths (callbacks, pixel conversion, resize, the frame-skip loop, recording, replay
checks, shard export and frame decoding, ROM hashing, RAM search) are Rust. The only C or C++ involved is third-party: the
cores we load and libraries bundled by crates (for example libzstd).
