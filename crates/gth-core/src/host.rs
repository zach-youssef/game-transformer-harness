//! The process-global host state and the callbacks a core calls.
//!
//! libretro callbacks carry no user pointer, so they can only reach the host through a global.
//! [`Claim`] makes sure at most one [`Core`](crate::Core) uses it at a time. The callbacks
//! lock [`HOST`] for each call and never panic: they are `extern "C"`, so a panic would abort
//! the process.

use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use gth_frame::PixelFormat;
use gth_libretro_sys::*;

/// What the callbacks read and write. Cleared when a core is unloaded.
pub(crate) struct Host {
    /// Answer to `GET_SYSTEM_DIRECTORY` (BIOS files).
    pub system_dir: Option<CString>,
    /// Answer to `GET_SAVE_DIRECTORY`: the loaded core's temporary save directory.
    pub save_dir: Option<CString>,
    pub format: PixelFormat,
    /// The last frame the core drew, rows packed (`width * bytes_per_pixel` apart). A dupe
    /// (`NULL` frame) keeps it.
    pub frame: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// Port 0 joypad buttons, bit n = button id n.
    pub buttons: u16,
    /// Audio frames the core produced since loading, counted and dropped (headless).
    pub audio_frames: u64,
    /// Core options from `SET_VARIABLES` with their current values.
    pub options: Vec<CoreOption>,
    /// Memory descriptors from `SET_MEMORY_MAPS`.
    pub regions: Vec<MemoryRegion>,
}

pub(crate) struct CoreOption {
    pub key: CString,
    pub value: CString,
}

/// A memory map descriptor, as plain data (the pointer as an address, so `Host` is `Send`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    /// `RETRO_MEMDESC_*` flags.
    pub flags: u64,
    /// First emulated address of the region.
    pub start: usize,
    /// Length in bytes.
    pub len: usize,
    pub(crate) host_address: usize,
}

impl Host {
    const EMPTY: Host = Host {
        system_dir: None,
        save_dir: None,
        format: PixelFormat::Rgb1555,
        frame: Vec::new(),
        width: 0,
        height: 0,
        buttons: 0,
        audio_frames: 0,
        options: Vec::new(),
        regions: Vec::new(),
    };

    /// A black frame of `width` x `height` in the current format, reusing the buffer.
    pub fn clear_frame(&mut self, width: usize, height: usize) {
        self.frame.clear();
        self.frame
            .resize(width * height * self.format.bytes_per_pixel(), 0);
        (self.width, self.height) = (width, height);
    }
}

static HOST: Mutex<Host> = Mutex::new(Host::EMPTY);

/// Whether a core holds the host. Separate from `HOST` so a failed claim doesn't touch it.
static CLAIMED: AtomicBool = AtomicBool::new(false);

pub(crate) fn host() -> MutexGuard<'static, Host> {
    // The callbacks never panic while holding the lock, and a panicking caller leaves the
    // state consistent between fields, so a poisoned lock is still usable.
    HOST.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The right to use the host: held by the one loaded [`Core`](crate::Core). Dropping it clears
/// the host, so a core loaded later starts fresh.
pub(crate) struct Claim(());

impl Claim {
    /// `None` if another core holds the host.
    pub fn take() -> Option<Claim> {
        CLAIMED
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        *host() = Host::EMPTY;
        Some(Claim(()))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        *host() = Host::EMPTY;
        CLAIMED.store(false, Ordering::Release);
    }
}

/// Maps a libretro pixel format to ours; `None` for formats we don't convert.
fn pixel_format(format: retro_pixel_format) -> Option<PixelFormat> {
    match format {
        RETRO_PIXEL_FORMAT_0RGB1555 => Some(PixelFormat::Rgb1555),
        RETRO_PIXEL_FORMAT_XRGB8888 => Some(PixelFormat::Xrgb8888),
        RETRO_PIXEL_FORMAT_RGB565 => Some(PixelFormat::Rgb565),
        _ => None,
    }
}

/// The default of a `SET_VARIABLES` definition `"Label; default|other|..."`.
fn option_default(definition: &CStr) -> CString {
    let bytes = definition.to_bytes();
    let values = bytes
        .windows(2)
        .position(|pair| pair == b"; ")
        .map_or(&[][..], |at| &bytes[at + 2..]);
    let default = values.split(|&b| b == b'|').next().unwrap_or_default();
    // A sub-slice of a C string has no NUL.
    CString::new(default).unwrap_or_default()
}

/// Answers the core's environment calls. Commands not handled here return `false`, which
/// cores treat as "not supported".
///
/// # Safety
/// The core passes `data` of the type `libretro.h` documents for `cmd` (gth-libretro-sys
/// names it on each constant), valid for the call.
pub(crate) unsafe extern "C" fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    let mut host = host();
    match cmd {
        RETRO_ENVIRONMENT_GET_CAN_DUPE => {
            // SAFETY: `bool *` (out).
            unsafe { data.cast::<bool>().write(true) };
            true
        }
        RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY | RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY => {
            let dir = if cmd == RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY {
                &host.system_dir
            } else {
                &host.save_dir
            };
            let dir = dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr());
            // SAFETY: `const char **` (out). The string lives in `HOST` until the core is
            // unloaded.
            unsafe { data.cast::<*const c_char>().write(dir) };
            true
        }
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => {
            // SAFETY: `const retro_pixel_format *` (in).
            let Some(format) = pixel_format(unsafe { data.cast::<retro_pixel_format>().read() })
            else {
                return false;
            };
            host.format = format;
            let (width, height) = (host.width, host.height);
            host.clear_frame(width, height);
            true
        }
        RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS | RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME => true,
        RETRO_ENVIRONMENT_SET_VARIABLES => {
            host.options.clear();
            let mut variable = data.cast::<retro_variable>().cast_const();
            // SAFETY: `const retro_variable *`, an array ended by `{ NULL, NULL }` whose
            // strings are C strings; we copy them.
            unsafe {
                while !(*variable).key.is_null() {
                    let entry = *variable;
                    if !entry.value.is_null() {
                        host.options.push(CoreOption {
                            key: CStr::from_ptr(entry.key).to_owned(),
                            value: option_default(CStr::from_ptr(entry.value)),
                        });
                    }
                    variable = variable.add(1);
                }
            }
            true
        }
        RETRO_ENVIRONMENT_GET_VARIABLE => {
            // SAFETY: `retro_variable *` (in: key, a C string; out: value).
            let variable = unsafe { &mut *data.cast::<retro_variable>() };
            if variable.key.is_null() {
                return false;
            }
            // SAFETY: as above.
            let key = unsafe { CStr::from_ptr(variable.key) };
            let option = host
                .options
                .iter()
                .find(|option| option.key.as_c_str() == key);
            // The value lives in `HOST` until the options are set again or the core unloads.
            variable.value = option.map_or(std::ptr::null(), |option| option.value.as_ptr());
            option.is_some()
        }
        RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE => {
            // SAFETY: `bool *` (out). The host never changes options behind the core's back.
            unsafe { data.cast::<bool>().write(false) };
            true
        }
        RETRO_ENVIRONMENT_SET_VARIABLE => {
            // A NULL `data` asks whether the command is supported.
            // SAFETY: `const retro_variable *` (in) or NULL; its strings are C strings.
            let Some(variable) = (unsafe { data.cast::<retro_variable>().as_ref() }) else {
                return true;
            };
            if variable.key.is_null() || variable.value.is_null() {
                return false;
            }
            // SAFETY: as above.
            let (key, value) =
                unsafe { (CStr::from_ptr(variable.key), CStr::from_ptr(variable.value)) };
            match host
                .options
                .iter_mut()
                .find(|option| option.key.as_c_str() == key)
            {
                Some(option) => {
                    option.value = value.to_owned();
                    true
                }
                None => false,
            }
        }
        RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION => {
            // SAFETY: `unsigned *` (out). 0: the core falls back to `SET_VARIABLES`.
            unsafe { data.cast::<c_uint>().write(0) };
            true
        }
        RETRO_ENVIRONMENT_SET_MEMORY_MAPS => {
            // SAFETY: `const retro_memory_map *` (in) with `num_descriptors` descriptors,
            // which we copy (the memory they point to lives until the game is unloaded).
            let descriptors = unsafe {
                let map = &*data.cast::<retro_memory_map>();
                if map.descriptors.is_null() {
                    &[]
                } else {
                    std::slice::from_raw_parts(map.descriptors, map.num_descriptors as usize)
                }
            };
            host.regions = descriptors
                .iter()
                .filter(|descriptor| !descriptor.ptr.is_null())
                .map(|descriptor| MemoryRegion {
                    flags: descriptor.flags,
                    start: descriptor.start,
                    len: descriptor.len,
                    host_address: descriptor.ptr as usize + descriptor.offset,
                })
                .collect();
            true
        }
        // No C shim for the variadic log callback yet (docs/libretro.md): cores then log to
        // stderr or not at all.
        RETRO_ENVIRONMENT_GET_LOG_INTERFACE => false,
        _ => false,
    }
}

/// Keeps the frame (packed) or, for a `NULL` dupe, the previous one.
///
/// # Safety
/// `data` is NULL or holds `height` rows of `width` pixels in the current format, `pitch`
/// bytes apart, valid for the call.
pub(crate) unsafe extern "C" fn video_refresh(
    data: *const c_void,
    width: c_uint,
    height: c_uint,
    pitch: usize,
) {
    if data.is_null() {
        return;
    }
    let mut host = host();
    let (width, height) = (width as usize, height as usize);
    let row = width * host.format.bytes_per_pixel();
    if height == 0 || pitch < row {
        return; // not a frame we can read; keep the last one
    }
    // SAFETY: the core's frame spans `pitch * (height - 1) + row` bytes from `data`.
    let src = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), pitch * (height - 1) + row) };
    host.frame.clear();
    for y in 0..height {
        host.frame.extend_from_slice(&src[y * pitch..][..row]);
    }
    (host.width, host.height) = (width, height);
}

pub(crate) extern "C" fn audio_sample(_left: i16, _right: i16) {
    host().audio_frames += 1;
}

/// # Safety
/// No requirement: the samples are counted, not read.
pub(crate) unsafe extern "C" fn audio_sample_batch(_data: *const i16, frames: usize) -> usize {
    host().audio_frames += frames as u64;
    frames
}

/// Input is set before each frame, so there is nothing to poll.
pub(crate) extern "C" fn input_poll() {}

/// Port 0's joypad buttons; every other port and device reads as idle.
pub(crate) extern "C" fn input_state(
    port: c_uint,
    device: c_uint,
    _index: c_uint,
    id: c_uint,
) -> i16 {
    if port != 0 || device != RETRO_DEVICE_JOYPAD {
        return 0;
    }
    let buttons = host().buttons;
    match id {
        // The mask's bit 15 lands in the sign bit, as in other frontends.
        RETRO_DEVICE_ID_JOYPAD_MASK => buttons as i16,
        0..16 => i16::from((buttons >> id) & 1 == 1),
        _ => 0,
    }
}

// The callbacks have the types the core expects.
const _: retro_environment_t = Some(environment);
const _: retro_video_refresh_t = Some(video_refresh);
const _: retro_audio_sample_t = Some(audio_sample);
const _: retro_audio_sample_batch_t = Some(audio_sample_batch);
const _: retro_input_poll_t = Some(input_poll);
const _: retro_input_state_t = Some(input_state);

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit tests share `HOST`, so each holds the claim (other tests wait for it).
    fn claim() -> Claim {
        loop {
            if let Some(claim) = Claim::take() {
                return claim;
            }
            std::thread::yield_now();
        }
    }

    fn set_format(format: retro_pixel_format) -> bool {
        let mut format = format;
        // SAFETY: SET_PIXEL_FORMAT takes a `const retro_pixel_format *`.
        unsafe { environment(RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, (&raw mut format).cast()) }
    }

    #[test]
    fn claim_is_exclusive_and_released_on_drop() {
        let first = claim();
        assert!(Claim::take().is_none());
        drop(first);
        drop(claim());
    }

    #[test]
    fn accepts_the_three_pixel_formats() {
        let _claim = claim();
        assert!(set_format(RETRO_PIXEL_FORMAT_RGB565));
        assert_eq!(host().format, PixelFormat::Rgb565);
        assert!(set_format(RETRO_PIXEL_FORMAT_XRGB8888));
        assert!(set_format(RETRO_PIXEL_FORMAT_0RGB1555));
        assert!(!set_format(3), "unknown format refused");
        assert_eq!(host().format, PixelFormat::Rgb1555);
    }

    #[test]
    fn video_packs_rows_and_keeps_the_frame_on_dupes() {
        let _claim = claim();
        assert!(set_format(RETRO_PIXEL_FORMAT_RGB565));
        // 2x2 pixels of 2 bytes, rows 6 bytes apart: 2 bytes of padding per row.
        let src: [u8; 10] = [1, 2, 3, 4, 0xEE, 0xEE, 5, 6, 7, 8];
        // SAFETY: `src` holds 2 rows of 2 RGB565 pixels, 6 bytes apart.
        unsafe { video_refresh(src.as_ptr().cast(), 2, 2, 6) };
        // SAFETY: a NULL frame is a dupe.
        unsafe { video_refresh(std::ptr::null(), 2, 2, 6) };
        let host = host();
        assert_eq!(host.frame, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!((host.width, host.height), (2, 2));
    }

    #[test]
    fn video_ignores_a_pitch_shorter_than_a_row() {
        let _claim = claim();
        host().clear_frame(1, 1);
        // SAFETY: the call returns before reading `data`.
        unsafe { video_refresh([0u8; 8].as_ptr().cast(), 2, 2, 2) };
        assert_eq!(host().frame, [0, 0]);
    }

    #[test]
    fn input_reads_port_0_joypad_and_mask() {
        let _claim = claim();
        host().buttons = 0b1000_0000_0001_0001;
        assert_eq!(input_state(0, RETRO_DEVICE_JOYPAD, 0, 0), 1);
        assert_eq!(input_state(0, RETRO_DEVICE_JOYPAD, 0, 1), 0);
        assert_eq!(input_state(0, RETRO_DEVICE_JOYPAD, 0, 15), 1);
        assert_eq!(input_state(0, RETRO_DEVICE_JOYPAD, 0, 16), 0);
        assert_eq!(
            input_state(0, RETRO_DEVICE_JOYPAD, 0, RETRO_DEVICE_ID_JOYPAD_MASK) as u16,
            0b1000_0000_0001_0001
        );
        assert_eq!(input_state(1, RETRO_DEVICE_JOYPAD, 0, 0), 0);
        assert_eq!(input_state(0, RETRO_DEVICE_NONE, 0, 0), 0);
    }

    #[test]
    fn answers_the_system_and_save_directories_separately() {
        let _claim = claim();
        let dir = |cmd| {
            let mut out: *const c_char = std::ptr::null();
            // SAFETY: both commands take a `const char **` (out).
            assert!(unsafe { environment(cmd, (&raw mut out).cast()) });
            // SAFETY: a non-NULL answer is a C string in `HOST`, alive while the claim is held.
            (!out.is_null()).then(|| unsafe { CStr::from_ptr(out) }.to_owned())
        };
        assert_eq!(dir(RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY), None);
        host().system_dir = Some(c"/bios".to_owned());
        host().save_dir = Some(c"/tmp/save".to_owned());
        assert_eq!(
            dir(RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY).as_deref(),
            Some(c"/bios")
        );
        assert_eq!(
            dir(RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY).as_deref(),
            Some(c"/tmp/save")
        );
    }

    #[test]
    fn options_take_their_defaults_and_can_be_set_by_the_core() {
        let _claim = claim();
        let definitions = [
            retro_variable {
                key: c"a".as_ptr(),
                value: c"Label A; x|y".as_ptr(),
            },
            retro_variable {
                key: c"b".as_ptr(),
                value: c"no values".as_ptr(),
            },
            retro_variable {
                key: std::ptr::null(),
                value: std::ptr::null(),
            },
        ];
        let get = |key: &CStr| {
            let mut variable = retro_variable {
                key: key.as_ptr(),
                value: std::ptr::null(),
            };
            // SAFETY: GET_VARIABLE takes a `retro_variable *`; the value is read before the
            // next environment call.
            let found =
                unsafe { environment(RETRO_ENVIRONMENT_GET_VARIABLE, (&raw mut variable).cast()) };
            // SAFETY: a non-NULL value is a C string owned by the host.
            found.then(|| unsafe { CStr::from_ptr(variable.value) }.to_owned())
        };
        // SAFETY: SET_VARIABLES takes a `{ NULL, NULL }`-terminated array.
        assert!(unsafe {
            environment(
                RETRO_ENVIRONMENT_SET_VARIABLES,
                definitions.as_ptr().cast_mut().cast(),
            )
        });
        assert_eq!(get(c"a").as_deref(), Some(c"x"));
        assert_eq!(get(c"b").as_deref(), Some(c""));
        assert_eq!(get(c"c"), None);
        let mut set = retro_variable {
            key: c"a".as_ptr(),
            value: c"y".as_ptr(),
        };
        // SAFETY: SET_VARIABLE takes a `const retro_variable *`.
        assert!(unsafe { environment(RETRO_ENVIRONMENT_SET_VARIABLE, (&raw mut set).cast()) });
        assert_eq!(get(c"a").as_deref(), Some(c"y"));
    }

    #[test]
    fn declines_the_log_interface_and_unknown_commands() {
        let _claim = claim();
        let mut log = retro_log_callback { log: None };
        // SAFETY: GET_LOG_INTERFACE takes a `retro_log_callback *`; it is declined untouched.
        assert!(!unsafe {
            environment(RETRO_ENVIRONMENT_GET_LOG_INTERFACE, (&raw mut log).cast())
        });
        assert!(log.log.is_none());
        // SAFETY: an unknown command is declined without reading `data`.
        assert!(!unsafe { environment(0xFFFF, std::ptr::null_mut()) });
    }
}
