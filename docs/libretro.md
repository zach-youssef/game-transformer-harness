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
  (`2|1|4` pixels, default 2) per frame, clamped to the screen.
- **Input**: port 0 joypad, queried per button (not with `RETRO_DEVICE_ID_JOYPAD_MASK`).
- **Audio**: 800 silent stereo frames per `retro_run`, through the batch callback.
- **RAM**: `RETRO_MEMORY_SYSTEM_RAM` is 256 bytes, also registered with `SET_MEMORY_MAPS` at
  address 0. Bytes 0-3 are the score, 8 packed BCD digits with the most significant byte
  first, which goes up by 1 on every frame A is held and wraps after 99999999. Bytes 4-7 are
  the last PRNG output (xorshift32 seeded with FNV-1a of the content, little-endian; zero
  without content). The rest stays zero. No save RAM.
- **Savestates**: 276 bytes, the frame number, square position, PRNG state and system RAM. The
  step option is not saved, and a state that is short or puts the square off screen is
  refused. `retro_reset` returns to the state right after loading.

Its unit tests (`src/tests.rs`) call the exported functions directly with a fake host behind
the callbacks.
