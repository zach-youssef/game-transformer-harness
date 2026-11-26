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
