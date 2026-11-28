# libretro FFI

`crates/gth-libretro-sys` is the hand-written FFI for the part of
[`libretro.h`](https://github.com/libretro/libretro-common/blob/master/include/libretro.h)
the harness uses. It has no bindgen and no build script, so CI needs no libclang. The header
commit it was transcribed from is in the crate's doc comment.

## What it holds

- `RETRO_API_VERSION` (1), joypad device and button ids, memory ids, region ids.
- Environment commands: pixel format, system and save directories, memory maps, legacy core
  options (`SET_VARIABLES`, `GET_VARIABLE`, `GET_VARIABLE_UPDATE`, `SET_VARIABLE`) and
  `GET_CORE_OPTIONS_VERSION`, input descriptors, log interface, `GET_CAN_DUPE`,
  `SET_SUPPORT_NO_GAME`. Each constant's doc comment names the type `data` points to.
- `RETRO_ENVIRONMENT_PRIVATE` and the harness's own command in that range,
  `GTH_ENVIRONMENT_SET_AREA_INTERFACE` with `gth_area_interface` (below, "Named areas"); not
  in `libretro.h`.
- The structs those commands and the entry points use, `#[repr(C)]`.
- The callback types the host passes to the core (`retro_*_t`, `Option<extern "C" fn>` so they
  are nullable), and one function-pointer type per exported entry point (`retro_*_fn`, the
  type of symbol `retro_*`) for the host to load by name.

Core options v1/v2 structs are not defined: a host answers `GET_CORE_OPTIONS_VERSION` with 0
and cores fall back to `SET_VARIABLES`. Cheats and `retro_load_game_special` are left out.

## Extending it

1. Copy the item from the header at the recorded commit (or a newer one, then update the commit
   in the crate doc and check the items already there didn't change).
2. Keep the C name. `unsigned` is `c_uint`, `bool` is `bool`, `size_t` is `usize`, enums whose
   range reaches `INT_MAX` are `c_int` aliases with constants, nullable function pointers are
   `Option<unsafe extern "C" fn(..)>`.
3. Say in the doc comment what `data` points to and who owns it.
4. Add new structs to the layout test (`struct_layouts_match_c`), with sizes and offsets worked
   out by hand from the C declaration.

C-variadic functions (the log callback) can be called from Rust but not defined, so a host
that wants core logs needs a small C shim; until then it answers `GET_LOG_INTERFACE` with
`false`.

## Host

`gth-core` (`src/host.rs`) answers these environment commands and declines the rest (`false`,
which cores read as "not supported"):

| Command | Answer |
|---|---|
| `GET_CAN_DUPE` | `true`: a `NULL` frame keeps the last one |
| `SET_PIXEL_FORMAT` | accepts `0RGB1555`, `XRGB8888` and `RGB565` |
| `GET_SYSTEM_DIRECTORY` | the `system_dir` given to `Core::load` (BIOS files, read-only in practice) |
| `GET_SAVE_DIRECTORY` | a fresh temporary directory per `Core`, removed when it drops, so NVRAM, high scores and memory cards never carry over between loads or race between processes |
| `SET_VARIABLES`, `GET_VARIABLE`, `SET_VARIABLE` | each option starts at the value `Core::load` was given for it (a game's `core_options`), or else its default (the first listed value); the core may set one. `Core::load` fails if a given option isn't declared or its value isn't listed |
| `GET_VARIABLE_UPDATE` | always `false`: the host never changes options |
| `GET_CORE_OPTIONS_VERSION` | 0 |
| `SET_MEMORY_MAPS` | copies the descriptors; `read_mapped` reads a region as contiguous from `start` (mirrors aren't resolved) |
| `GTH_ENVIRONMENT_SET_AREA_INTERFACE` | copies the core's `get_area` (below, "Named areas"); `false` for another `version` |
| `SET_INPUT_DESCRIPTORS`, `SET_SUPPORT_NO_GAME` | `true`, ignored |
| `GET_LOG_INTERFACE` | `false` (no C shim, see above) |

RAM variables (docs/games.md) are read with `Core::read_ram`, from a core memory
(`retro_get_memory_data`) or the memory map, and fail with `Error::Ram` naming the variable
when the bytes aren't there. `Core::check_ram_vars` checks that every variable's bytes are
there without decoding them; memory sizes and maps are fixed once the game is loaded, so
after that check a read can fail only on bytes that don't decode. `Core::run_inputs` plays a scripted input sequence, such as a
game's reset script.

Input is port 0's joypad only, answered per button and for `RETRO_DEVICE_ID_JOYPAD_MASK`.
Audio is counted and dropped, unless `Core::set_audio_capture(true)` keeps the interleaved
stereo samples for `Core::take_audio` (gth-play plays them). Content is read into memory unless the core sets
`need_fullpath`, and stays alive until the game is unloaded; an unreadable file is
`Error::Content`. A `need_fullpath` core gets only the path, which needn't exist: FBNeo
loads a clone in a merged set from `<dir>/<clone>.zip` through its parent's zip.

### Named areas

Some labels live in a core's own buffers, outside system RAM and the memory map: Psikyo's
buffered sprite lists, for instance. Our FBNeo
fork (docs/cores.md, "Forks") and the test core hand the host a lookup of their state areas
with a private environment command, `GTH_ENVIRONMENT_SET_AREA_INTERFACE`
(`RETRO_ENVIRONMENT_PRIVATE | 0x4754`, in the range `libretro.h` reserves for frontend- and
core-private commands), at `retro_load_game`:

```c
struct gth_area_interface {
    unsigned version;  /* 1 */
    bool (*get_area)(const char *name, const void **data, size_t *size);
};
```

The host copies the struct. `get_area` returns the one area with that name, `false` for none
or several; in FBNeo the areas are what `BurnAcb` sees when a savestate is taken (a scan with
neither `ACB_READ` nor `ACB_WRITE`, like `CheevosInit`'s, which copies nothing), named by
`ScanVar`/`BurnArea::szName`. The command changes nothing else, and a frontend that doesn't
know it answers `false`, so the fork stays a drop-in core.

`Core::area(name)` and `Core::areas([names])` give the bytes, borrowed from the `Core` like
`Core::memory`; `None` when the core gave no interface (`Core::has_areas`), has no such area or
several. Each name is looked up once per load and its address cached, so use only areas whose
storage is fixed while the game is loaded (allocated at init, or static), not a pointer a
driver moves between banks.

`Core::reset` restores a savestate taken right after loading, so every reset starts from the
same state. `retro_reset` is the console's reset button, and many cores keep RAM or NVRAM
through it, so it is only the fallback for cores that can't serialize at load time (some
report a savestate size of 0 until their first frame) or that refuse that state on a reset
(the snapshot is then dropped); `Core::resets_to_load_state` says which one a core gets.
`Core::soft_reset` calls `retro_reset` whatever the core: the start of a chained episode
(docs/evaluation.md, `start.later_episodes`), which continues from where the last one ended.
It runs no settle frames (below), which follow only savestate loads: a chained episode of a
game with `settle_frames` skips them (the ALE games have none).

`Core::load` takes the game's `settle_frames` (docs/games.md, "Settle frames"): after every
savestate load, the load snapshot's included, the core runs that many frames with no button
held and loads the state again, for cores whose first frame after a load would otherwise
depend on what ran before it. Settle frames aren't counted in `frames_run` or
`audio_frames`, their audio is dropped, and the frame is cleared after them as after any
load. `settle_frames_run_unseen_after_every_load` (crates/gth-core/tests/testcore.rs) checks
this on the test core's lag option, at load, on reset and on `unserialize`.

## Test core

`crates/gth-testcore` is a deterministic libretro core (a `cdylib`) that stands in for real
cores in tests and CI: video, input, RAM, core options and savestates, with no ROM.

- **Content** is optional (`SET_SUPPORT_NO_GAME`). Content bytes, passed in memory
  (`need_fullpath = false`, extension `gthc`), only seed the PRNG.
- **Video**: 320x240 `XRGB8888` at 60 fps; `retro_load_game` fails if the host refuses the
  format. Each frame: the background's green is the low byte of the frame number (the number
  of `retro_run` calls since load or reset, so the first frame is 1) and its blue is the PRNG
  state's low byte (0 without content); a strip of 16 cells, 20x8 each, across the top, cell
  `id` coloured `0xFF0000 | id * 0x1010` while joypad button `id` is held; a 16x16 white
  square, starting at (152, 112), that the d-pad moves by the `gth_testcore_step` core option
  (`2|1|4` pixels, default 2) per frame, clamped to the screen. With the `gth_testcore_dupe`
  core option `on` (default `off`) and a host that answers `GET_CAN_DUPE`, a frame with no
  button held sends no new picture (`NULL`, repeat the previous one), as cores do on static
  screens.
  With the `gth_testcore_lag` core option `on` (default `off`), the background's red is the
  low 7 bits of the previous frame's number (drawn or repeated; 0 when none has run since
  load), which savestates don't hold: like FBNeo's Toaplan drivers, which draw with the
  palette computed after the previous frame, the first frame after a load shows what the
  process ran before it (docs/games.md, `settle_frames`).
- **Sprites**: with the `gth_testcore_sprites` core option `on` (default `off`, read at load),
  the core registers the area interface (above, "Named areas") with one area, `gth_testcore
  sprites`, and keeps a buffered marker list: after each frame (and on load, reset and
  `unserialize`) it lists a 4x4 marker at the square's centre and one at (4 + 6i, 10) for each
  of the first 8 lives (byte 8 of RAM), and the next frame draws them magenta (`0xFF00FF`) on
  top. The list derives from what savestates hold, so a load restores it. Layout: a count
  byte, then 9 entries of x, y (i16) and width, height (u8), little-endian. As with Psikyo's
  lists, reading it between frames gives what the next frame draws. With the option `off`
  nothing changes.
- **Input**: port 0 joypad, queried per button (not with `RETRO_DEVICE_ID_JOYPAD_MASK`).
- **Audio**: 800 silent stereo frames per `retro_run`, through the batch callback.
- **RAM**: `RETRO_MEMORY_SYSTEM_RAM` is 256 bytes, also registered with `SET_MEMORY_MAPS` at
  address 0. Bytes 0-3 are the score, 8 packed BCD digits with the most significant byte
  first, which goes up by 1 on every frame A is held and wraps after 99999999. Bytes 4-7 are
  the last PRNG output (xorshift32 seeded with FNV-1a of the content, little-endian; zero
  without content). Byte 8 is a lives counter for rewind tests: +1 (wrapping) on every frame
  Select is held, then -1 (stopping at 0) on every frame L is held. The rest stays zero. No
  save RAM.
- **Savestates**: 276 bytes, the frame number, square position, PRNG state and system RAM. The
  options are not saved, and a state that is short or puts the square off screen is
  refused. `retro_reset` is a soft reset, like a console's reset button: the frame number and
  the square start over, but RAM (so the score) and the PRNG keep their values.

- **One instance per process**: like real cores, it keeps its state in process-globals
  (libretro callbacks carry no user pointer), so one process can't run two instances of the
  same core file. The host enforces one core per process anyway (architecture.md).

Its unit tests (`src/tests.rs`) call the exported functions directly with a fake host behind
the callbacks. `tests/cdylib.rs` loads the built library by path, as the host will, to check
the file name and the exports on each OS.

### Loading the test core in tests

Tests that load the core as a library get its path from `gth_test_support::testcore_path()`
(`crates/gth-test-support`, a dev-dependency). `cargo test` doesn't build a cdylib-only
package's library, so the helper runs `cargo build -p gth-testcore` itself (with `--release`
when the tests are optimized; not `--target`) and takes the path from cargo's JSON output,
which also works with any `CARGO_TARGET_DIR`.

Python tests use the session fixture `testcore_path` (`tests/conftest.py`), which follows the
same rule (`cargo build -p gth-testcore --message-format=json`, path from the JSON) before any
test loads the core. It runs cargo directly rather than through a Rust helper under
`cargo run`, whose nested build could wait on the outer cargo's lock on Windows. pytest runs
in one process, so every test that loads the core closes it, even when it fails (the `env`
fixture or the `retro_env` context manager in `tests/test_env.py`): a leaked core makes every
later load fail with "another core is loaded".

On Windows a DLL that a process has loaded can't be overwritten, so nothing may rebuild the
core while a test has it loaded. The helper builds once per process, before the first load,
and `cargo test` runs test binaries one at a time, each finding the build fresh (a no-op).
Don't run core-loading test binaries in parallel processes (as `cargo nextest` does) while
the core's sources change.
