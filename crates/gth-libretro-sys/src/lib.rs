//! Hand-written FFI for the subset of `libretro.h` the harness uses (docs/libretro.md).
//!
//! Transcribed from libretro-common `include/libretro.h` at commit
//! `ae22debd425c33fe355ac2e51d9a140d7e8b3fa6` (2026-11-21), API version 1. Names match the
//! header so that a grep finds both. Add items from the same header as they are needed, and
//! update the commit above if you read a newer one.
//!
//! C `unsigned` is `c_uint`, C `bool` is Rust `bool`, and C enums whose range reaches `INT_MAX`
//! are `c_int`. `RETRO_CALLCONV` is cdecl on 32-bit x86 and the only convention elsewhere, which
//! is what `extern "C"` means.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_uint, c_void};

pub const RETRO_API_VERSION: c_uint = 1;

// Devices and joypad buttons (`retro_input_state_t`'s `device` and `id`).
pub const RETRO_DEVICE_NONE: c_uint = 0;
pub const RETRO_DEVICE_JOYPAD: c_uint = 1;

pub const RETRO_DEVICE_ID_JOYPAD_B: c_uint = 0;
pub const RETRO_DEVICE_ID_JOYPAD_Y: c_uint = 1;
pub const RETRO_DEVICE_ID_JOYPAD_SELECT: c_uint = 2;
pub const RETRO_DEVICE_ID_JOYPAD_START: c_uint = 3;
pub const RETRO_DEVICE_ID_JOYPAD_UP: c_uint = 4;
pub const RETRO_DEVICE_ID_JOYPAD_DOWN: c_uint = 5;
pub const RETRO_DEVICE_ID_JOYPAD_LEFT: c_uint = 6;
pub const RETRO_DEVICE_ID_JOYPAD_RIGHT: c_uint = 7;
pub const RETRO_DEVICE_ID_JOYPAD_A: c_uint = 8;
pub const RETRO_DEVICE_ID_JOYPAD_X: c_uint = 9;
pub const RETRO_DEVICE_ID_JOYPAD_L: c_uint = 10;
pub const RETRO_DEVICE_ID_JOYPAD_R: c_uint = 11;
pub const RETRO_DEVICE_ID_JOYPAD_L2: c_uint = 12;
pub const RETRO_DEVICE_ID_JOYPAD_R2: c_uint = 13;
pub const RETRO_DEVICE_ID_JOYPAD_L3: c_uint = 14;
pub const RETRO_DEVICE_ID_JOYPAD_R3: c_uint = 15;
/// Asks for all 16 buttons at once as a bitmask (bit n = button id n). Hosts must also
/// answer per-button queries, which most cores use.
pub const RETRO_DEVICE_ID_JOYPAD_MASK: c_uint = 256;

pub const RETRO_REGION_NTSC: c_uint = 0;
pub const RETRO_REGION_PAL: c_uint = 1;

// Memory ids for `retro_get_memory_data` / `retro_get_memory_size`.
pub const RETRO_MEMORY_MASK: c_uint = 0xff;
pub const RETRO_MEMORY_SAVE_RAM: c_uint = 0;
pub const RETRO_MEMORY_RTC: c_uint = 1;
pub const RETRO_MEMORY_SYSTEM_RAM: c_uint = 2;
pub const RETRO_MEMORY_VIDEO_RAM: c_uint = 3;

// Environment commands: `retro_environment_t`'s `cmd`, with the type `data` points to.
pub const RETRO_ENVIRONMENT_EXPERIMENTAL: c_uint = 0x10000;
/// `bool *` (out): whether `retro_video_refresh_t` accepts `NULL` to repeat the last frame.
pub const RETRO_ENVIRONMENT_GET_CAN_DUPE: c_uint = 3;
/// `const char **` (out): the BIOS directory, or `NULL`.
pub const RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY: c_uint = 9;
/// `const retro_pixel_format *` (in). The host returns `false` to refuse a format.
pub const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: c_uint = 10;
/// `const retro_input_descriptor *` (in): array ended by an entry with `description == NULL`.
pub const RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS: c_uint = 11;
/// `retro_variable *` (in: key, out: value or `NULL`): read a core option.
pub const RETRO_ENVIRONMENT_GET_VARIABLE: c_uint = 15;
/// `const retro_variable *` (in): option definitions `"Label; default|other|..."`, ended by
/// `{ NULL, NULL }`. The host copies them.
pub const RETRO_ENVIRONMENT_SET_VARIABLES: c_uint = 16;
/// `bool *` (out): whether any option changed since the last `GET_VARIABLE`.
pub const RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE: c_uint = 17;
/// `const bool *` (in): the core runs without content.
pub const RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME: c_uint = 18;
/// `retro_log_callback *` (out).
pub const RETRO_ENVIRONMENT_GET_LOG_INTERFACE: c_uint = 27;
/// `const char **` (out): the save directory, or `NULL`.
pub const RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY: c_uint = 31;
/// `const retro_memory_map *` (in). The host copies the descriptors (not the memory).
pub const RETRO_ENVIRONMENT_SET_MEMORY_MAPS: c_uint = 36 | RETRO_ENVIRONMENT_EXPERIMENTAL;
/// `unsigned *` (out). A host that answers 0 gets the legacy `SET_VARIABLES` from cores
/// instead of the core options v1/v2 structs, which this crate doesn't define.
pub const RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION: c_uint = 52;
/// `const retro_variable *` (in): the core sets one of its own options.
pub const RETRO_ENVIRONMENT_SET_VARIABLE: c_uint = 70;

// Memory descriptor flags (`retro_memory_descriptor::flags`).
pub const RETRO_MEMDESC_CONST: u64 = 1 << 0;
pub const RETRO_MEMDESC_BIGENDIAN: u64 = 1 << 1;
pub const RETRO_MEMDESC_SYSTEM_RAM: u64 = 1 << 2;
pub const RETRO_MEMDESC_SAVE_RAM: u64 = 1 << 3;
pub const RETRO_MEMDESC_VIDEO_RAM: u64 = 1 << 4;

pub type retro_pixel_format = c_int;
pub const RETRO_PIXEL_FORMAT_0RGB1555: retro_pixel_format = 0;
/// 32 bits per pixel, native-endian `0x00RRGGBB`.
pub const RETRO_PIXEL_FORMAT_XRGB8888: retro_pixel_format = 1;
pub const RETRO_PIXEL_FORMAT_RGB565: retro_pixel_format = 2;

pub type retro_log_level = c_int;
pub const RETRO_LOG_DEBUG: retro_log_level = 0;
pub const RETRO_LOG_INFO: retro_log_level = 1;
pub const RETRO_LOG_WARN: retro_log_level = 2;
pub const RETRO_LOG_ERROR: retro_log_level = 3;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct retro_memory_descriptor {
    pub flags: u64,
    /// Start of the block in host memory; `start`/`select`/`disconnect` map emulated addresses.
    pub ptr: *mut c_void,
    pub offset: usize,
    pub start: usize,
    pub select: usize,
    pub disconnect: usize,
    pub len: usize,
    pub addrspace: *const c_char,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct retro_memory_map {
    pub descriptors: *const retro_memory_descriptor,
    pub num_descriptors: c_uint,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct retro_variable {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct retro_input_descriptor {
    pub port: c_uint,
    pub device: c_uint,
    pub index: c_uint,
    pub id: c_uint,
    pub description: *const c_char,
}

/// A printf-style logger. Rust can call it but can't define one (C-variadic definitions are
/// unstable), so a host that wants core logs needs a small C shim.
pub type retro_log_printf_t =
    Option<unsafe extern "C" fn(level: retro_log_level, fmt: *const c_char, ...)>;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct retro_log_callback {
    pub log: retro_log_printf_t,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct retro_system_info {
    pub library_name: *const c_char,
    pub library_version: *const c_char,
    /// `|`-separated extensions without dots, e.g. `"nes|fds"`.
    pub valid_extensions: *const c_char,
    /// `true`: `retro_load_game` gets a path, not the content in memory.
    pub need_fullpath: bool,
    pub block_extract: bool,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct retro_game_geometry {
    pub base_width: c_uint,
    pub base_height: c_uint,
    pub max_width: c_uint,
    pub max_height: c_uint,
    /// 0.0 means `base_width / base_height`.
    pub aspect_ratio: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct retro_system_timing {
    pub fps: f64,
    pub sample_rate: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct retro_system_av_info {
    pub geometry: retro_game_geometry,
    pub timing: retro_system_timing,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct retro_game_info {
    /// UTF-8 path, or `NULL`.
    pub path: *const c_char,
    /// The content in memory (`NULL` when the core set `need_fullpath`).
    pub data: *const c_void,
    pub size: usize,
    pub meta: *const c_char,
}

// Callbacks the host gives the core. `Option` makes them nullable, as C function pointers are.
pub type retro_environment_t = Option<unsafe extern "C" fn(cmd: c_uint, data: *mut c_void) -> bool>;
/// `data` is `NULL` to repeat the previous frame (only if the host answered `GET_CAN_DUPE`).
pub type retro_video_refresh_t =
    Option<unsafe extern "C" fn(data: *const c_void, width: c_uint, height: c_uint, pitch: usize)>;
pub type retro_audio_sample_t = Option<unsafe extern "C" fn(left: i16, right: i16)>;
/// `data` holds `frames` interleaved stereo samples; returns how many frames the host took.
pub type retro_audio_sample_batch_t =
    Option<unsafe extern "C" fn(data: *const i16, frames: usize) -> usize>;
pub type retro_input_poll_t = Option<unsafe extern "C" fn()>;
pub type retro_input_state_t =
    Option<unsafe extern "C" fn(port: c_uint, device: c_uint, index: c_uint, id: c_uint) -> i16>;

// Entry points the core exports, as function-pointer types for the host to load them by name
// (each `retro_*_fn` is the type of the symbol `retro_*`). Cheats and `retro_load_game_special`
// are left out.
pub type retro_set_environment_fn = unsafe extern "C" fn(retro_environment_t);
pub type retro_set_video_refresh_fn = unsafe extern "C" fn(retro_video_refresh_t);
pub type retro_set_audio_sample_fn = unsafe extern "C" fn(retro_audio_sample_t);
pub type retro_set_audio_sample_batch_fn = unsafe extern "C" fn(retro_audio_sample_batch_t);
pub type retro_set_input_poll_fn = unsafe extern "C" fn(retro_input_poll_t);
pub type retro_set_input_state_fn = unsafe extern "C" fn(retro_input_state_t);
pub type retro_init_fn = unsafe extern "C" fn();
pub type retro_deinit_fn = unsafe extern "C" fn();
pub type retro_api_version_fn = unsafe extern "C" fn() -> c_uint;
pub type retro_get_system_info_fn = unsafe extern "C" fn(*mut retro_system_info);
pub type retro_get_system_av_info_fn = unsafe extern "C" fn(*mut retro_system_av_info);
pub type retro_set_controller_port_device_fn = unsafe extern "C" fn(port: c_uint, device: c_uint);
pub type retro_reset_fn = unsafe extern "C" fn();
pub type retro_run_fn = unsafe extern "C" fn();
pub type retro_serialize_size_fn = unsafe extern "C" fn() -> usize;
pub type retro_serialize_fn = unsafe extern "C" fn(data: *mut c_void, len: usize) -> bool;
pub type retro_unserialize_fn = unsafe extern "C" fn(data: *const c_void, len: usize) -> bool;
/// `game` is `NULL` when loading without content (`SET_SUPPORT_NO_GAME`).
pub type retro_load_game_fn = unsafe extern "C" fn(game: *const retro_game_info) -> bool;
pub type retro_unload_game_fn = unsafe extern "C" fn();
pub type retro_get_region_fn = unsafe extern "C" fn() -> c_uint;
/// Valid until `retro_unload_game`; `NULL` if the core has no such memory.
pub type retro_get_memory_data_fn = unsafe extern "C" fn(id: c_uint) -> *mut c_void;
pub type retro_get_memory_size_fn = unsafe extern "C" fn(id: c_uint) -> usize;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    // Layouts a host and a C core must agree on, for the 64-bit targets we build (CI and the
    // Mac). 32-bit x86 aligns doubles in structs to 4, so `timing` would move there.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn struct_layouts_match_c() {
        assert_eq!(size_of::<retro_memory_descriptor>(), 64);
        assert_eq!(offset_of!(retro_memory_map, num_descriptors), 8);
        assert_eq!(size_of::<retro_input_descriptor>(), 24);
        assert_eq!(offset_of!(retro_system_info, need_fullpath), 24);
        assert_eq!(offset_of!(retro_system_info, block_extract), 25);
        assert_eq!(offset_of!(retro_system_av_info, timing), 24);
        assert_eq!(size_of::<retro_system_av_info>(), 40);
        assert_eq!(size_of::<retro_game_info>(), 32);
        assert_eq!(size_of::<retro_log_printf_t>(), 8);
    }
}
