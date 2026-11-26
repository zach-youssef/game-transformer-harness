//! Calls the exported entry points directly, with a fake host behind the callbacks.
//!
//! The fake host's callbacks are `extern "C"`, so a panic can't unwind out of them: a failing
//! `assert!` inside one aborts the whole test binary instead of failing one test. Look for
//! the panic message above the abort.

use super::*;

/// What the fake host saw and what it answers. Tests share the core's globals, so each one
/// holds `SERIAL` through a `Session`.
struct Host {
    buttons: u16,
    step: &'static CStr,
    step_changed: bool,
    pixel_format: Option<retro_pixel_format>,
    memory_map: Option<retro_memory_descriptor>,
    step_defined: bool,
    frame: Vec<u32>,
    audio_frames: usize,
    polls: usize,
}

// SAFETY: `memory_map`'s raw pointers are only compared, never dereferenced through `HOST`.
unsafe impl Send for Host {}

static HOST: Mutex<Host> = Mutex::new(Host::new());
static SERIAL: Mutex<()> = Mutex::new(());

impl Host {
    const fn new() -> Self {
        Host {
            buttons: 0,
            step: c"2",
            step_changed: false,
            pixel_format: None,
            memory_map: None,
            step_defined: false,
            frame: Vec::new(),
            audio_frames: 0,
            polls: 0,
        }
    }
}

fn host() -> MutexGuard<'static, Host> {
    lock(&HOST)
}

/// The core passes `data` of the type documented for `cmd`, and the strings in it are valid
/// C strings, which every `SAFETY` below relies on.
unsafe extern "C" fn environment_cb(cmd: c_uint, data: *mut c_void) -> bool {
    let mut host = host();
    match cmd {
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => {
            // SAFETY: `const retro_pixel_format *`.
            host.pixel_format = Some(unsafe { *data.cast::<retro_pixel_format>() });
        }
        RETRO_ENVIRONMENT_SET_MEMORY_MAPS => {
            // SAFETY: `const retro_memory_map *` with `num_descriptors` descriptors.
            let descriptors = unsafe {
                let map = &*data.cast::<retro_memory_map>();
                std::slice::from_raw_parts(map.descriptors, map.num_descriptors as usize)
            };
            assert_eq!(descriptors.len(), 1);
            host.memory_map = Some(descriptors[0]);
        }
        RETRO_ENVIRONMENT_SET_VARIABLES => {
            // SAFETY: `const retro_variable *`, a non-empty array.
            let key = unsafe { CStr::from_ptr((*data.cast::<retro_variable>()).key) };
            host.step_defined = key == STEP_KEY;
        }
        RETRO_ENVIRONMENT_GET_VARIABLE => {
            // SAFETY: `retro_variable *`; `host.step` is 'static.
            let variable = unsafe { &mut *data.cast::<retro_variable>() };
            // SAFETY: the key is a C string.
            assert_eq!(unsafe { CStr::from_ptr(variable.key) }, STEP_KEY);
            variable.value = host.step.as_ptr();
            host.step_changed = false;
        }
        RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE => {
            // SAFETY: `bool *`.
            unsafe { *data.cast::<bool>() = host.step_changed };
        }
        RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME => {}
        _ => return false,
    }
    true
}

unsafe extern "C" fn video_cb(data: *const c_void, width: c_uint, height: c_uint, pitch: usize) {
    assert_eq!((width, height, pitch), (320, 240, 320 * 4));
    // SAFETY: the core passes height rows of `pitch` bytes, and pitch is width pixels.
    let pixels = unsafe { std::slice::from_raw_parts(data.cast::<u32>(), WIDTH * HEIGHT) };
    host().frame = pixels.to_vec();
}

unsafe extern "C" fn audio_batch_cb(_data: *const i16, frames: usize) -> usize {
    host().audio_frames += frames;
    frames
}

unsafe extern "C" fn input_poll_cb() {
    host().polls += 1;
}

unsafe extern "C" fn input_state_cb(
    port: c_uint,
    device: c_uint,
    _index: c_uint,
    id: c_uint,
) -> i16 {
    assert_eq!((port, device), (0, RETRO_DEVICE_JOYPAD));
    ((host().buttons >> id) & 1) as i16
}

/// A loaded core with the fake host attached; unloads on drop.
struct Session(#[allow(dead_code)] MutexGuard<'static, ()>);

impl Session {
    fn start(content: Option<&[u8]>) -> Self {
        let serial = lock(&SERIAL);
        *host() = Host::new();
        // SAFETY: the callbacks are 'static functions with the libretro signatures.
        unsafe { retro_set_environment(Some(environment_cb)) };
        retro_set_video_refresh(Some(video_cb));
        retro_set_audio_sample_batch(Some(audio_batch_cb));
        retro_set_input_poll(Some(input_poll_cb));
        retro_set_input_state(Some(input_state_cb));
        retro_init();
        let info = content.map(|bytes| retro_game_info {
            path: std::ptr::null(),
            data: bytes.as_ptr().cast(),
            size: bytes.len(),
            meta: std::ptr::null(),
        });
        let info_ptr = info.as_ref().map_or(std::ptr::null(), std::ptr::from_ref);
        // SAFETY: `info_ptr` is null or points to `info`, whose data outlives the call.
        assert!(unsafe { retro_load_game(info_ptr) });
        Session(serial)
    }

    /// Runs one frame with `buttons` held and returns it.
    fn run(&self, buttons: u16) -> Vec<u32> {
        host().buttons = buttons;
        retro_run();
        host().frame.clone()
    }

    fn ram(&self) -> Vec<u8> {
        let size = retro_get_memory_size(RETRO_MEMORY_SYSTEM_RAM);
        // SAFETY: the core's system RAM is `size` bytes and lives until unload.
        unsafe {
            std::slice::from_raw_parts(retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM).cast(), size)
        }
        .to_vec()
    }

    fn save(&self) -> Vec<u8> {
        let mut state = vec![0; retro_serialize_size()];
        // SAFETY: `state` is writable for its length.
        assert!(unsafe { retro_serialize(state.as_mut_ptr().cast(), state.len()) });
        state
    }

    fn load(&self, state: &[u8]) -> bool {
        // SAFETY: `state` is readable for its length.
        unsafe { retro_unserialize(state.as_ptr().cast(), state.len()) }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        retro_unload_game();
        retro_deinit();
    }
}

const fn bit(id: c_uint) -> u16 {
    1 << id
}
const UP: u16 = bit(RETRO_DEVICE_ID_JOYPAD_UP);
const LEFT: u16 = bit(RETRO_DEVICE_ID_JOYPAD_LEFT);
const RIGHT: u16 = bit(RETRO_DEVICE_ID_JOYPAD_RIGHT);
const A: u16 = bit(RETRO_DEVICE_ID_JOYPAD_A);
const START: u16 = bit(RETRO_DEVICE_ID_JOYPAD_START);

/// Top-left corner of the white square.
fn square(frame: &[u32]) -> (usize, usize) {
    let index = frame
        .iter()
        .position(|&p| p == SQUARE_COLOUR)
        .expect("square drawn");
    (index % WIDTH, index / WIDTH)
}

#[test]
fn reports_api_and_av_info() {
    assert_eq!(retro_api_version(), 1);
    let mut av = retro_system_av_info::default();
    // SAFETY: `av` is writable.
    unsafe { retro_get_system_av_info(&mut av) };
    assert_eq!(
        (av.geometry.base_width, av.geometry.base_height),
        (320, 240)
    );
    assert_eq!((av.timing.fps, av.timing.sample_rate), (60.0, 48_000.0));
    let mut info = std::mem::MaybeUninit::<retro_system_info>::uninit();
    // SAFETY: `info` is writable, and fully written by the call.
    let info = unsafe {
        retro_get_system_info(info.as_mut_ptr());
        info.assume_init()
    };
    // SAFETY: the core returns 'static C strings.
    let name = unsafe { CStr::from_ptr(info.library_name) };
    assert_eq!(name, c"gth-testcore");
    assert!(!info.need_fullpath);
}

#[test]
fn load_sets_format_options_and_memory_map() {
    let session = Session::start(None);
    let host = host();
    assert_eq!(host.pixel_format, Some(RETRO_PIXEL_FORMAT_XRGB8888));
    assert!(host.step_defined);
    let map = host.memory_map.expect("memory map set");
    assert_eq!(map.ptr, retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM));
    assert_eq!(
        (map.flags, map.start, map.len),
        (RETRO_MEMDESC_SYSTEM_RAM, 0, 256)
    );
    drop(host);
    assert_eq!(session.ram(), vec![0; 256]);
    assert_eq!(retro_get_memory_size(RETRO_MEMORY_SAVE_RAM), 0);
}

#[test]
fn frame_shows_frame_number_buttons_and_square() {
    let session = Session::start(None);
    let frame = session.run(A | START);
    assert_eq!(frame.len(), WIDTH * HEIGHT);
    assert_eq!(square(&frame), (152, 112));
    let cell = |id: usize| frame[id * 20 + 10];
    assert_eq!(cell(8), button_colour(8));
    assert_eq!(cell(3), button_colour(3));
    assert_eq!(cell(0), 0x0100, "unheld cell shows the background");
    assert_eq!(frame[WIDTH * 100], 0x0100, "green = frame number 1");
    assert_eq!(session.run(0)[WIDTH * 100], 0x0200);
    let host = host();
    assert_eq!((host.polls, host.audio_frames), (2, 1600));
}

#[test]
fn dpad_moves_square_by_the_step_option_and_clamps() {
    let session = Session::start(None);
    assert_eq!(square(&session.run(RIGHT)), (154, 112));
    assert_eq!(square(&session.run(RIGHT | UP)), (156, 110));
    {
        let mut host = host();
        host.step = c"4";
        host.step_changed = true;
    }
    assert_eq!(square(&session.run(RIGHT)), (160, 110));
    for _ in 0..100 {
        session.run(LEFT | UP);
    }
    assert_eq!(square(&session.run(0)), (0, 0));
}

#[test]
fn score_counts_frames_with_a_held_in_bcd() {
    let session = Session::start(None);
    for _ in 0..12 {
        session.run(A);
    }
    session.run(0);
    assert_eq!(session.ram()[..4], [0x00, 0x00, 0x00, 0x12]);
    let ram = retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM).cast::<[u8; 4]>();
    // SAFETY: system RAM is 256 bytes and no reference to it is live.
    unsafe { ram.write([0x99, 0x99, 0x99, 0x99]) };
    session.run(A);
    assert_eq!(session.ram()[..4], [0; 4], "wraps after 99999999");
}

#[test]
fn bcd_increment_carries() {
    let mut digits = [0x00, 0x99, 0x99];
    bcd_increment(&mut digits);
    assert_eq!(digits, [0x01, 0x00, 0x00]);
}

#[test]
fn savestate_restores_frames_and_ram() {
    let session = Session::start(Some(b"content"));
    let inputs = [A, RIGHT, A | UP, 0, LEFT, A, A | START, RIGHT];
    for &buttons in &inputs {
        session.run(buttons);
    }
    let state = session.save();
    assert_eq!(state.len(), STATE_SIZE);
    let play = || inputs.map(|buttons| (session.run(buttons), session.ram()));
    let first = play();
    assert!(session.load(&state));
    assert!(
        first == play(),
        "same frames and RAM after loading the state"
    );
    assert!(
        !session.load(&state[..STATE_SIZE - 1]),
        "short state refused"
    );
}

#[test]
fn corrupted_savestate_is_refused() {
    let session = Session::start(None);
    session.run(RIGHT);
    let good = session.save();
    let mut bad = good.clone();
    bad[8..12].copy_from_slice(&i32::MAX.to_le_bytes()); // x
    assert!(!session.load(&bad));
    let mut bad = good.clone();
    bad[12..16].copy_from_slice(&(-1i32).to_le_bytes()); // y
    assert!(!session.load(&bad));
    assert_eq!(square(&session.run(0)), (154, 112), "state unchanged");
    let mut last_frame = good;
    last_frame[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(session.load(&last_frame));
    assert_eq!(session.run(0)[WIDTH * 100], 0, "frame number wraps to 0");
}

#[test]
fn reset_returns_to_the_loaded_state() {
    let session = Session::start(Some(b"content"));
    let ram_ptr = retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM);
    let first = (session.run(A | RIGHT), session.ram());
    session.run(A);
    retro_reset();
    assert!(first == (session.run(A | RIGHT), session.ram()));
    assert_eq!(
        retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM),
        ram_ptr,
        "RAM didn't move"
    );
}

#[test]
fn prng_is_seeded_by_content() {
    let rng_outputs = |content: Option<&[u8]>| {
        let session = Session::start(content);
        (0..3)
            .map(|_| {
                session.run(0);
                session.ram()[RNG_OUT..RNG_OUT + 4].to_vec()
            })
            .collect::<Vec<_>>()
    };
    let abc = rng_outputs(Some(b"abc"));
    assert_eq!(abc, rng_outputs(Some(b"abc")));
    assert_ne!(abc, rng_outputs(Some(b"abd")));
    assert_ne!(abc[0], abc[1]);
    assert_eq!(rng_outputs(None), vec![vec![0; 4]; 3]);
}
