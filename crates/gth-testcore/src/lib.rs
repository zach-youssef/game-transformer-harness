//! A tiny deterministic libretro core that tests and CI load instead of real cores and ROMs.
//!
//! What it draws, where the score lives and what a savestate holds: docs/libretro.md,
//! "Test core". libretro calls a core from one thread at a time, and the core keeps its
//! state in process-globals because the callbacks carry no user pointer. Like a real core, it
//! is a singleton: one process can't run two instances of the same core file.

use std::ffi::{CStr, c_char, c_uint, c_void};
use std::sync::{Mutex, MutexGuard, PoisonError};

use gth_libretro_sys::*;

const WIDTH: usize = 320;
const HEIGHT: usize = 240;
const SQUARE: usize = 16;
const SQUARE_COLOUR: u32 = 0x00FF_FFFF;
/// Height of the strip of 16 button cells, each `WIDTH / 16` wide, at the top of the frame.
const STRIP: usize = 8;
const RAM_SIZE: usize = 0x100;
/// RAM offset of the score: 8 BCD digits in 4 bytes, most significant byte first.
const SCORE: usize = 0;
/// RAM offset of the last PRNG output, a little-endian u32 (zero without content).
const RNG_OUT: usize = 4;
/// RAM offset of a lives counter for rewind tests: Select held adds one, L held takes one
/// (stopping at zero); nothing else changes it.
const LIVES: usize = 8;
/// frame (u64), x, y (i32), rng (u32), RAM; all little-endian.
const STATE_SIZE: usize = 8 + 4 + 4 + 4 + RAM_SIZE;
const SAMPLE_RATE: usize = 48_000;
const FPS: usize = 60;
static SILENCE: [i16; 2 * SAMPLE_RATE / FPS] = [0; 2 * SAMPLE_RATE / FPS];

const STEP_KEY: &CStr = c"gth_testcore_step";
const DUPE_KEY: &CStr = c"gth_testcore_dupe";
const LAG_KEY: &CStr = c"gth_testcore_lag";
const VERSION: &CStr =
    match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
        Ok(version) => version,
        Err(_) => panic!("version has a NUL"),
    };

/// The colour of button `id`'s cell while it is held. Red is 0xFF, which the background never
/// has, and each id gets its own green and blue.
const fn button_colour(id: usize) -> u32 {
    0x00FF_0000 | (id as u32 * 0x1010)
}

#[derive(Clone, Copy)]
struct Callbacks {
    environment: retro_environment_t,
    video: retro_video_refresh_t,
    audio_batch: retro_audio_sample_batch_t,
    input_poll: retro_input_poll_t,
    input_state: retro_input_state_t,
}

const NO_CALLBACKS: Callbacks = Callbacks {
    environment: None,
    video: None,
    audio_batch: None,
    input_poll: None,
    input_state: None,
};

static CALLBACKS: Mutex<Callbacks> = Mutex::new(NO_CALLBACKS);

/// The loaded game. Never locked while a host callback runs, so the host may call back into
/// the core (for example `retro_get_memory_data` from its video callback).
static GAME: Mutex<Option<Game>> = Mutex::new(None);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Game {
    /// `retro_run` calls since load or reset.
    frame: u64,
    x: i32,
    y: i32,
    /// xorshift32 state seeded from the content; 0 means no PRNG (loaded without content).
    rng: u32,
    /// Boxed so the pointer handed to the host stays put until `retro_unload_game`.
    ram: Box<[u8; RAM_SIZE]>,
    /// Core option `gth_testcore_step`: pixels the square moves per frame. Not saved, as
    /// options aren't in real cores' savestates either.
    step: i32,
    /// Core option `gth_testcore_dupe`: on frames with no button held, send no new frame
    /// (`NULL`, "repeat the previous one"), if the host allows it. Not saved either.
    dupe: bool,
    /// Core option `gth_testcore_lag`: draw the background's red from `previous`, as FBNeo's
    /// Toaplan drivers draw with the palette they computed after the previous frame.
    lag: bool,
    /// The low 7 bits of the previous frame's number, set after each frame (drawn or not) and
    /// 0 at load: state the core derives while running and its savestates don't hold.
    previous: u8,
    pixels: Vec<u32>,
}

impl Game {
    fn new(seed: u32, options: Options) -> Self {
        let mut game = Game {
            frame: 0,
            x: 0,
            y: 0,
            rng: seed,
            ram: Box::new([0; RAM_SIZE]),
            step: options.step,
            dupe: options.dupe,
            lag: options.lag,
            previous: 0,
            pixels: vec![0; WIDTH * HEIGHT],
        };
        game.reset();
        game
    }

    /// A soft reset, like a console's reset button: the frame number and the square start
    /// over, but the RAM (score) and the PRNG keep their values.
    fn reset(&mut self) {
        self.frame = 0;
        self.x = ((WIDTH - SQUARE) / 2) as i32;
        self.y = ((HEIGHT - SQUARE) / 2) as i32;
    }

    fn update(&mut self, buttons: u16) {
        let held = |id: c_uint| i32::from((buttons >> id) & 1 == 1);
        self.frame = self.frame.wrapping_add(1);
        let dx = held(RETRO_DEVICE_ID_JOYPAD_RIGHT) - held(RETRO_DEVICE_ID_JOYPAD_LEFT);
        let dy = held(RETRO_DEVICE_ID_JOYPAD_DOWN) - held(RETRO_DEVICE_ID_JOYPAD_UP);
        self.x = (self.x + dx * self.step).clamp(0, (WIDTH - SQUARE) as i32);
        self.y = (self.y + dy * self.step).clamp(0, (HEIGHT - SQUARE) as i32);
        if held(RETRO_DEVICE_ID_JOYPAD_A) == 1 {
            bcd_increment(&mut self.ram[SCORE..SCORE + 4]);
        }
        let lives = &mut self.ram[LIVES];
        if held(RETRO_DEVICE_ID_JOYPAD_SELECT) == 1 {
            *lives = lives.wrapping_add(1);
        }
        if held(RETRO_DEVICE_ID_JOYPAD_L) == 1 {
            *lives = lives.saturating_sub(1);
        }
        if self.rng != 0 {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 17;
            self.rng ^= self.rng << 5;
            self.ram[RNG_OUT..RNG_OUT + 4].copy_from_slice(&self.rng.to_le_bytes());
        }
    }

    /// Background: green is the frame number's low byte, blue the PRNG's, and with the lag
    /// option red is `previous` (below 0x80, so never a button cell's 0xFF).
    fn render(&mut self, buttons: u16) {
        let red = if self.lag {
            u32::from(self.previous)
        } else {
            0
        };
        let background = red << 16 | u32::from(self.frame as u8) << 8 | u32::from(self.rng as u8);
        self.pixels.fill(background);
        let cell = WIDTH / 16;
        for id in (0..16).filter(|id| (buttons >> id) & 1 == 1) {
            for row in 0..STRIP {
                let start = row * WIDTH + id * cell;
                self.pixels[start..start + cell].fill(button_colour(id));
            }
        }
        let (x, y) = (self.x as usize, self.y as usize);
        for row in y..y + SQUARE {
            self.pixels[row * WIDTH + x..row * WIDTH + x + SQUARE].fill(SQUARE_COLOUR);
        }
    }

    fn save(&self) -> Vec<u8> {
        let mut state = Vec::with_capacity(STATE_SIZE);
        state.extend_from_slice(&self.frame.to_le_bytes());
        state.extend_from_slice(&self.x.to_le_bytes());
        state.extend_from_slice(&self.y.to_le_bytes());
        state.extend_from_slice(&self.rng.to_le_bytes());
        state.extend_from_slice(&self.ram[..]);
        state
    }

    /// `None`, changing nothing, if `state` is short or has the square off screen.
    fn restore(&mut self, state: &[u8]) -> Option<()> {
        let (frame, state) = state.split_first_chunk()?;
        let (x, state) = state.split_first_chunk()?;
        let (y, state) = state.split_first_chunk()?;
        let (rng, state) = state.split_first_chunk()?;
        let (ram, _) = state.split_first_chunk()?;
        let x = i32::from_le_bytes(*x);
        let y = i32::from_le_bytes(*y);
        if !(0..=(WIDTH - SQUARE) as i32).contains(&x)
            || !(0..=(HEIGHT - SQUARE) as i32).contains(&y)
        {
            return None;
        }
        self.frame = u64::from_le_bytes(*frame);
        (self.x, self.y) = (x, y);
        self.rng = u32::from_le_bytes(*rng);
        *self.ram = *ram;
        Some(())
    }
}

/// Adds 1 to big-endian packed BCD, wrapping from all nines to zero.
fn bcd_increment(digits: &mut [u8]) {
    for byte in digits.iter_mut().rev() {
        let (high, low) = (*byte >> 4, *byte & 0xF);
        if low < 9 {
            *byte = high << 4 | (low + 1);
            return;
        }
        if high < 9 {
            *byte = (high + 1) << 4;
            return;
        }
        *byte = 0;
    }
}

/// FNV-1a of the content, never 0 (0 turns the PRNG off).
fn seed_from(content: &[u8]) -> u32 {
    let hash = content.iter().fold(0x811C_9DC5_u32, |hash, &byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    hash.max(1)
}

/// Calls the host's environment callback; `false` if there is none or it declines `cmd`.
///
/// # Safety
/// `data` points to what the header documents for `cmd`, valid for the call.
unsafe fn environment<T>(cmd: c_uint, data: *mut T) -> bool {
    let Some(callback) = lock(&CALLBACKS).environment else {
        return false;
    };
    // SAFETY: the host gave us this callback for exactly this use, and the caller guarantees
    // `data` matches `cmd`.
    unsafe { callback(cmd, data.cast()) }
}

/// The core options' values.
#[derive(Clone, Copy)]
struct Options {
    step: i32,
    dupe: bool,
    lag: bool,
}

/// An option's value, or `None` if the host doesn't have it.
fn read_option(key: &CStr) -> Option<&'static [u8]> {
    let mut variable = retro_variable {
        key: key.as_ptr(),
        value: std::ptr::null(),
    };
    // SAFETY: GET_VARIABLE takes a `retro_variable *`; `variable` outlives the call.
    if !unsafe { environment(RETRO_ENVIRONMENT_GET_VARIABLE, &mut variable) }
        || variable.value.is_null()
    {
        return None;
    }
    // SAFETY: the host set `value` to a NUL-terminated string it keeps alive at least until
    // the next environment call, and the value is only compared before then.
    Some(unsafe { CStr::from_ptr(variable.value) }.to_bytes())
}

/// The options, with the defaults (step 2, dupe and lag off) for any the host doesn't have. Dupe is
/// also off if the host can't repeat frames (`GET_CAN_DUPE`).
fn read_options() -> Options {
    let step = match read_option(STEP_KEY) {
        Some(b"1") => 1,
        Some(b"4") => 4,
        _ => 2,
    };
    let mut can_dupe = false;
    // SAFETY: GET_CAN_DUPE takes a `bool *`; `can_dupe` outlives the call.
    let can_dupe =
        unsafe { environment(RETRO_ENVIRONMENT_GET_CAN_DUPE, &mut can_dupe) } && can_dupe;
    let dupe = can_dupe && read_option(DUPE_KEY) == Some(b"on");
    let lag = read_option(LAG_KEY) == Some(b"on");
    Options { step, dupe, lag }
}

// Entry points, in header order. Signatures are checked against gth-libretro-sys at the end.

/// # Safety
/// `callback` is null or callable until the core is unloaded.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_set_environment(callback: retro_environment_t) {
    lock(&CALLBACKS).environment = callback;
    let mut no_game = true;
    // SAFETY: SET_SUPPORT_NO_GAME takes a `const bool *`; `no_game` outlives the call.
    unsafe { environment(RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME, &mut no_game) };
    let mut variables = [
        retro_variable {
            key: STEP_KEY.as_ptr(),
            value: c"Square step in pixels; 2|1|4".as_ptr(),
        },
        retro_variable {
            key: DUPE_KEY.as_ptr(),
            value: c"Repeat the frame while no button is held; off|on".as_ptr(),
        },
        retro_variable {
            key: LAG_KEY.as_ptr(),
            value: c"Draw red from the previous frame, which states don't hold; off|on".as_ptr(),
        },
        retro_variable {
            key: std::ptr::null(),
            value: std::ptr::null(),
        },
    ];
    // SAFETY: SET_VARIABLES takes a `{NULL, NULL}`-terminated `retro_variable` array, which
    // the host copies; the strings are 'static.
    unsafe { environment(RETRO_ENVIRONMENT_SET_VARIABLES, variables.as_mut_ptr()) };
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_video_refresh(callback: retro_video_refresh_t) {
    lock(&CALLBACKS).video = callback;
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_audio_sample(_callback: retro_audio_sample_t) {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_audio_sample_batch(callback: retro_audio_sample_batch_t) {
    lock(&CALLBACKS).audio_batch = callback;
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_input_poll(callback: retro_input_poll_t) {
    lock(&CALLBACKS).input_poll = callback;
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_input_state(callback: retro_input_state_t) {
    lock(&CALLBACKS).input_state = callback;
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_init() {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_deinit() {
    *lock(&GAME) = None;
    // The library may stay loaded for a later session, which must not reach this host.
    *lock(&CALLBACKS) = NO_CALLBACKS;
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_api_version() -> c_uint {
    RETRO_API_VERSION
}

/// # Safety
/// `info` is valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_get_system_info(info: *mut retro_system_info) {
    // SAFETY: the caller passes a writable `retro_system_info`; the strings are 'static.
    unsafe {
        info.write(retro_system_info {
            library_name: c"gth-testcore".as_ptr(),
            library_version: VERSION.as_ptr(),
            valid_extensions: c"gthc".as_ptr(),
            need_fullpath: false,
            block_extract: false,
        });
    }
}

/// # Safety
/// `info` is valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_get_system_av_info(info: *mut retro_system_av_info) {
    let geometry = retro_game_geometry {
        base_width: WIDTH as c_uint,
        base_height: HEIGHT as c_uint,
        max_width: WIDTH as c_uint,
        max_height: HEIGHT as c_uint,
        aspect_ratio: 4.0 / 3.0,
    };
    let timing = retro_system_timing {
        fps: FPS as f64,
        sample_rate: SAMPLE_RATE as f64,
    };
    // SAFETY: the caller passes a writable `retro_system_av_info`.
    unsafe { info.write(retro_system_av_info { geometry, timing }) };
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_controller_port_device(_port: c_uint, _device: c_uint) {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_reset() {
    if let Some(game) = lock(&GAME).as_mut() {
        game.reset();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_run() {
    let callbacks = *lock(&CALLBACKS);
    if let Some(poll) = callbacks.input_poll {
        // SAFETY: a host callback with no arguments.
        unsafe { poll() };
    }
    let mut buttons = 0u16;
    if let Some(state) = callbacks.input_state {
        for id in 0..16 {
            // SAFETY: a host callback taking plain integers.
            if unsafe { state(0, RETRO_DEVICE_JOYPAD, 0, id) } != 0 {
                buttons |= 1 << id;
            }
        }
    }
    let mut options_changed = false;
    // SAFETY: GET_VARIABLE_UPDATE takes a `bool *`; `options_changed` outlives the call.
    let options =
        (unsafe { environment(RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE, &mut options_changed) }
            && options_changed)
            .then(read_options);

    let pixels = {
        let mut guard = lock(&GAME);
        let Some(game) = guard.as_mut() else { return };
        if let Some(options) = options {
            (game.step, game.dupe, game.lag) = (options.step, options.dupe, options.lag);
        }
        game.update(buttons);
        let pixels = if game.dupe && buttons == 0 {
            std::ptr::null()
        } else {
            game.render(buttons);
            game.pixels.as_ptr()
        };
        game.previous = (game.frame & 0x7F) as u8;
        pixels
    };
    if let Some(video) = callbacks.video {
        // SAFETY: `pixels` is null (repeat the frame, which the host allowed) or WIDTH *
        // HEIGHT XRGB8888 pixels, rows WIDTH * 4 bytes apart. The buffer is only freed when
        // GAME is replaced or cleared (`retro_load_game`, `retro_unload_game`,
        // `retro_deinit`), none of which a host may call from inside its video callback.
        unsafe { video(pixels.cast(), WIDTH as c_uint, HEIGHT as c_uint, WIDTH * 4) };
    }
    if let Some(audio) = callbacks.audio_batch {
        // SAFETY: SILENCE holds SAMPLE_RATE / FPS interleaved stereo frames and is 'static.
        unsafe { audio(SILENCE.as_ptr(), SAMPLE_RATE / FPS) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_serialize_size() -> usize {
    STATE_SIZE
}

/// # Safety
/// `data` is valid for writes of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_serialize(data: *mut c_void, len: usize) -> bool {
    let guard = lock(&GAME);
    let Some(game) = guard.as_ref() else {
        return false;
    };
    if len < STATE_SIZE {
        return false;
    }
    // SAFETY: the caller guarantees `len >= STATE_SIZE` writable bytes at `data`.
    let out = unsafe { std::slice::from_raw_parts_mut(data.cast::<u8>(), STATE_SIZE) };
    out.copy_from_slice(&game.save());
    true
}

/// # Safety
/// `data` is valid for reads of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_unserialize(data: *const c_void, len: usize) -> bool {
    let mut guard = lock(&GAME);
    let Some(game) = guard.as_mut() else {
        return false;
    };
    if len < STATE_SIZE {
        return false;
    }
    // SAFETY: the caller guarantees `len >= STATE_SIZE` readable bytes at `data`.
    let state = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), STATE_SIZE) };
    game.restore(state).is_some()
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_cheat_reset() {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_cheat_set(_index: c_uint, _enabled: bool, _code: *const c_char) {}

/// # Safety
/// `game` is null or points to a valid `retro_game_info` whose `data` holds `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_load_game(game: *const retro_game_info) -> bool {
    let mut format = RETRO_PIXEL_FORMAT_XRGB8888;
    // SAFETY: SET_PIXEL_FORMAT takes a `const retro_pixel_format *`; `format` outlives it.
    if !unsafe { environment(RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, &mut format) } {
        return false;
    }
    // SAFETY: the caller guarantees `game` is null or valid.
    let content = match unsafe { game.as_ref() } {
        Some(info) if !info.data.is_null() && info.size > 0 => {
            // SAFETY: the caller guarantees `data` holds `size` bytes for this call.
            unsafe { std::slice::from_raw_parts(info.data.cast::<u8>(), info.size) }
        }
        _ => &[],
    };
    let seed = if content.is_empty() {
        0
    } else {
        seed_from(content)
    };

    let mut new_game = Game::new(seed, read_options());
    let ram = new_game.ram.as_mut_ptr();
    *lock(&GAME) = Some(new_game);
    let mut descriptor = retro_memory_descriptor {
        flags: RETRO_MEMDESC_SYSTEM_RAM,
        ptr: ram.cast(),
        offset: 0,
        start: 0,
        select: 0,
        disconnect: 0,
        len: RAM_SIZE,
        addrspace: std::ptr::null(),
    };
    let mut map = retro_memory_map {
        descriptors: &mut descriptor,
        num_descriptors: 1,
    };
    // SAFETY: SET_MEMORY_MAPS takes a `const retro_memory_map *`; the host copies the map, and
    // the RAM it points to lives until `retro_unload_game`.
    unsafe { environment(RETRO_ENVIRONMENT_SET_MEMORY_MAPS, &mut map) };
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_load_game_special(
    _game_type: c_uint,
    _info: *const retro_game_info,
    _num_info: usize,
) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_unload_game() {
    *lock(&GAME) = None;
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_get_region() -> c_uint {
    RETRO_REGION_NTSC
}

/// System RAM (`RETRO_MEMORY_SYSTEM_RAM`) while a game is loaded, else null.
#[unsafe(no_mangle)]
pub extern "C" fn retro_get_memory_data(id: c_uint) -> *mut c_void {
    match lock(&GAME).as_mut() {
        Some(game) if id == RETRO_MEMORY_SYSTEM_RAM => game.ram.as_mut_ptr().cast(),
        _ => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_get_memory_size(id: c_uint) -> usize {
    match lock(&GAME).as_ref() {
        Some(_) if id == RETRO_MEMORY_SYSTEM_RAM => RAM_SIZE,
        _ => 0,
    }
}

// The exports have the types the host will load them as.
const _: retro_set_environment_fn = retro_set_environment;
const _: retro_set_video_refresh_fn = retro_set_video_refresh;
const _: retro_set_audio_sample_fn = retro_set_audio_sample;
const _: retro_set_audio_sample_batch_fn = retro_set_audio_sample_batch;
const _: retro_set_input_poll_fn = retro_set_input_poll;
const _: retro_set_input_state_fn = retro_set_input_state;
const _: retro_init_fn = retro_init;
const _: retro_deinit_fn = retro_deinit;
const _: retro_api_version_fn = retro_api_version;
const _: retro_get_system_info_fn = retro_get_system_info;
const _: retro_get_system_av_info_fn = retro_get_system_av_info;
const _: retro_set_controller_port_device_fn = retro_set_controller_port_device;
const _: retro_reset_fn = retro_reset;
const _: retro_run_fn = retro_run;
const _: retro_serialize_size_fn = retro_serialize_size;
const _: retro_serialize_fn = retro_serialize;
const _: retro_unserialize_fn = retro_unserialize;
const _: retro_load_game_fn = retro_load_game;
const _: retro_unload_game_fn = retro_unload_game;
const _: retro_get_region_fn = retro_get_region;
const _: retro_get_memory_data_fn = retro_get_memory_data;
const _: retro_get_memory_size_fn = retro_get_memory_size;

#[cfg(test)]
mod tests;
