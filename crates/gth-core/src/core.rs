//! [`Core`]: one loaded libretro core and its game.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use gth_config::{Location, RamVar};
use gth_frame::FrameRef;
use gth_libretro_sys::*;
use libloading::Library;

use crate::host::{self, Claim, MemoryRegion};
use crate::{Error, Result};

/// The core's name and version (recorded in every dataset), from `retro_get_system_info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemInfo {
    pub library_name: String,
    pub library_version: String,
    /// `|`-separated content extensions without dots.
    pub valid_extensions: String,
    /// The core reads content from its path rather than from memory.
    pub need_fullpath: bool,
}

/// Frame geometry and timing, from `retro_get_system_av_info` after loading the game.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvInfo {
    pub base_width: u32,
    pub base_height: u32,
    pub max_width: u32,
    pub max_height: u32,
    /// Display aspect ratio; `base_width / base_height` when the core gives none.
    pub aspect_ratio: f32,
    pub fps: f64,
    pub sample_rate: f64,
}

/// The entry points, copied out of the library. Valid while `Core::library` is loaded.
struct Api {
    deinit: retro_deinit_fn,
    reset: retro_reset_fn,
    run: retro_run_fn,
    serialize_size: retro_serialize_size_fn,
    serialize: retro_serialize_fn,
    unserialize: retro_unserialize_fn,
    unload_game: retro_unload_game_fn,
    get_memory_data: retro_get_memory_data_fn,
    get_memory_size: retro_get_memory_size_fn,
}

/// The entry points used only while loading.
struct Setup {
    set_environment: retro_set_environment_fn,
    set_video_refresh: retro_set_video_refresh_fn,
    set_audio_sample: retro_set_audio_sample_fn,
    set_audio_sample_batch: retro_set_audio_sample_batch_fn,
    set_input_poll: retro_set_input_poll_fn,
    set_input_state: retro_set_input_state_fn,
    init: retro_init_fn,
    get_system_info: retro_get_system_info_fn,
    get_system_av_info: retro_get_system_av_info_fn,
    load_game: retro_load_game_fn,
    set_controller_port_device: retro_set_controller_port_device_fn,
}

/// A loaded libretro core with its game, run headless.
///
/// # One core per process
///
/// libretro callbacks carry no user pointer, so they reach the host through a process-global
/// (`host.rs`), and cores keep their own state in globals too. Loading takes a process-wide
/// claim on that global: while a `Core` exists, [`Core::load`] fails, and dropping the `Core`
/// releases it. Parallel environments are separate processes.
///
/// # Threads
///
/// libretro calls a core from one thread at a time. Every method that calls into the core
/// takes `&mut self`, so the borrow checker enforces that; a `Core` may move between threads
/// between calls (`Send`). Callbacks lock the host global, so a core that calls them from its
/// own threads is still memory-safe.
pub struct Core {
    path: PathBuf,
    api: Api,
    system_info: SystemInfo,
    av_info: AvInfo,
    /// `retro_run` calls since loading, not counting settle frames.
    frames_run: u64,
    /// Neutral frames run after each savestate load before loading it again
    /// ([`load`](Self::load)).
    settle_frames: u32,
    /// Whether `retro_init` and `retro_load_game` succeeded, for `Drop`.
    initialized: bool,
    game_loaded: bool,
    /// The savestate taken right after loading, which [`reset`](Self::reset) restores; `None`
    /// if the core couldn't serialize then.
    load_state: Option<Vec<u8>>,
    /// Areas looked up by [`areas`](Self::areas): address and length, or `None` if missing.
    areas_found: HashMap<String, Option<(usize, usize)>>,
    /// The content and its path, kept alive while the game is loaded: some cores keep the
    /// pointers `retro_load_game` gets.
    _content: Option<(CString, Vec<u8>)>,
    // Declared last, so they drop after `Drop::drop` has unloaded the game: the save
    // directory is removed, the library closed, then the claim released.
    save_dir: SaveDir,
    _library: Library,
    _claim: Claim,
}

/// Loads `symbol` from `library` as a `T`.
///
/// # Safety
/// `T` is the symbol's type.
unsafe fn symbol<T: Copy>(library: &Library, path: &Path, name: &str) -> Result<T> {
    // SAFETY: the caller guarantees the type.
    unsafe { library.get::<T>(name.as_bytes()) }
        .map(|symbol| *symbol)
        .map_err(|error| load_error(path, format!("missing {name}: {error}")))
}

/// A fresh directory for one core's save files (`GET_SAVE_DIRECTORY`), removed on drop, so
/// nothing a core saves (NVRAM, high scores, memory cards) carries over to a later load or
/// races with another process.
struct SaveDir(PathBuf);

impl SaveDir {
    fn create() -> std::io::Result<SaveDir> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir();
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("gth-save-{}-{n}", std::process::id()));
            // A leftover from a crashed process with the same id is skipped, not reused.
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(SaveDir(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for SaveDir {
    fn drop(&mut self) {
        // Best effort: a leftover temporary directory is harmless.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn load_error(path: &Path, reason: impl Into<String>) -> Error {
    Error::CoreLoad {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

fn path_cstring(path: &Path) -> Option<CString> {
    CString::new(path.to_str()?).ok()
}

/// A C string from the core, or "" for NULL.
///
/// # Safety
/// `ptr` is NULL or a C string valid for the call.
unsafe fn core_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees a C string.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// Fails unless every option the caller chose is one the core declared and took.
fn check_options(core_path: &Path, options: &BTreeMap<String, String>) -> Result<()> {
    let host = host::host();
    for (key, value) in options {
        match host
            .options
            .iter()
            .find(|o| o.key.to_bytes() == key.as_bytes())
        {
            None => {
                return Err(load_error(
                    core_path,
                    format!("the core has no option {key}"),
                ));
            }
            Some(option) if option.value.to_bytes() != value.as_bytes() => {
                return Err(load_error(
                    core_path,
                    format!("core option {key} = {value:?} isn't one of the core's values"),
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

impl Core {
    /// Loads the core at `core_path` and its content (`None` for cores that run without
    /// content), with `system_dir` as the BIOS directory. The core saves into a fresh
    /// temporary directory, removed when the `Core` drops ([`save_dir`](Self::save_dir)).
    /// After loading, the core's state is snapshotted for [`reset`](Self::reset).
    ///
    /// `options` sets core options (`SET_VARIABLES` keys) to one of the values the core lists
    /// for them, before the core reads them; the others keep the core's defaults.
    ///
    /// `settle_frames` (a game's `settle_frames`, docs/games.md) makes the first frame after
    /// a savestate load independent of what this process ran before: some cores keep state
    /// they derive while running outside their savestates (FBNeo's Toaplan and CAVE drivers
    /// draw each frame with the palette computed after the previous one), so that frame would
    /// show the previous run. After every load ([`unserialize`](Self::unserialize),
    /// [`reset`](Self::reset), and the load state taken here) the core runs `settle_frames`
    /// frames with no button held and loads the state again. Those frames aren't counted in
    /// [`frames_run`](Self::frames_run) or [`audio_frames`](Self::audio_frames), and their
    /// audio is dropped. 0 loads states as they are.
    ///
    /// Fails with [`Error::CoreLoad`] if another `Core` exists in this process, the library
    /// or one of its entry points is missing, its API version isn't 1, the content can't be
    /// read, the core refuses it, or an option isn't one the core declares with that value.
    pub fn load(
        core_path: &Path,
        content_path: Option<&Path>,
        system_dir: &Path,
        options: &BTreeMap<String, String>,
        settle_frames: u32,
    ) -> Result<Core> {
        let (mut core, setup) = Self::open(core_path, content_path, system_dir, options)?;
        core.settle_frames = settle_frames;

        let game = core._content.as_ref().map(|(path, data)| retro_game_info {
            path: path.as_ptr(),
            data: if data.is_empty() {
                std::ptr::null()
            } else {
                data.as_ptr().cast()
            },
            size: data.len(),
            meta: std::ptr::null(),
        });
        let game_ptr = game.as_ref().map_or(std::ptr::null(), std::ptr::from_ref);
        // SAFETY: `game_ptr` is NULL (no content) or points to `game`, whose strings and data
        // live in `core._content` until the game is unloaded.
        if !unsafe { (setup.load_game)(game_ptr) } {
            return Err(load_error(core_path, "the core refused the content"));
        }
        core.game_loaded = true;
        check_options(core_path, options)?;

        let mut av = retro_system_av_info::default();
        // SAFETY: `av` is writable; plain data.
        unsafe { (setup.get_system_av_info)(&mut av) };
        let geometry = av.geometry;
        core.av_info = AvInfo {
            base_width: geometry.base_width,
            base_height: geometry.base_height,
            max_width: geometry.max_width,
            max_height: geometry.max_height,
            aspect_ratio: if geometry.aspect_ratio > 0.0 {
                geometry.aspect_ratio
            } else {
                geometry.base_width as f32 / geometry.base_height.max(1) as f32
            },
            fps: av.timing.fps,
            sample_rate: av.timing.sample_rate,
        };
        // SAFETY: plain integers; port 0 is a joypad, which most cores assume anyway.
        unsafe { (setup.set_controller_port_device)(0, RETRO_DEVICE_JOYPAD) };
        core.load_state = core.serialize().ok();
        if settle_frames > 0
            && let Some(state) = core.load_state.take()
        {
            // The first episode starts as later resets do. A refused state leaves
            // `load_state` empty, as `reset` would.
            if core.settle(&state) {
                core.load_state = Some(state);
            }
        }
        core.clear_frame();
        Ok(core)
    }

    /// Loads the core at `core_path` without a game and unloads it again: `retro_init`,
    /// `retro_get_system_info`, `retro_deinit`. A check that the library loads on this
    /// machine (its dependencies are there) and reports the expected version, as `gth cores`
    /// does (docs/cores.md); `system_dir` is the BIOS directory, as for [`load`](Self::load).
    ///
    /// Fails with [`Error::CoreLoad`] as `load` does before it loads content.
    pub fn probe(core_path: &Path, system_dir: &Path) -> Result<SystemInfo> {
        let (core, _) = Self::open(core_path, None, system_dir, &BTreeMap::new())?;
        Ok(core.system_info.clone())
    }

    /// The part of [`load`](Self::load) before the game: claims the process's core slot,
    /// opens the library, reads its system info and the content, and runs `retro_init`.
    /// Dropping the `Core` deinitializes it.
    fn open(
        core_path: &Path,
        content_path: Option<&Path>,
        system_dir: &Path,
        options: &BTreeMap<String, String>,
    ) -> Result<(Core, Setup)> {
        let claim = Claim::take()
            .ok_or_else(|| load_error(core_path, "another core is loaded in this process"))?;
        let system_dir = path_cstring(system_dir).ok_or_else(|| {
            load_error(
                core_path,
                format!(
                    "system dir {} isn't UTF-8 without NULs",
                    system_dir.display()
                ),
            )
        })?;
        let save_dir = SaveDir::create()
            .map_err(|error| load_error(core_path, format!("cannot create a save dir: {error}")))?;
        let save_dir_c = path_cstring(&save_dir.0).ok_or_else(|| {
            load_error(
                core_path,
                format!("save dir {} isn't UTF-8 without NULs", save_dir.0.display()),
            )
        })?;
        let mut overrides = Vec::with_capacity(options.len());
        for (key, value) in options {
            let (Ok(key), Ok(value)) = (CString::new(key.as_str()), CString::new(value.as_str()))
            else {
                return Err(load_error(
                    core_path,
                    format!("core option {key} has a NUL"),
                ));
            };
            overrides.push(host::CoreOption { key, value });
        }
        {
            let mut host = host::host();
            host.system_dir = Some(system_dir);
            host.save_dir = Some(save_dir_c);
            host.option_overrides = overrides;
        }

        // SAFETY: loading a library runs its initialisers. Which core to trust is the user's
        // choice (cores.lock); libretro cores do nothing harmful at load time.
        let library = unsafe { Library::new(core_path) }
            .map_err(|error| load_error(core_path, error.to_string()))?;
        // SAFETY: gth-libretro-sys's `retro_*_fn` is the type of the symbol `retro_*` in
        // libretro.h. The same holds for every `symbol` call below.
        let api_version: retro_api_version_fn =
            unsafe { symbol(&library, core_path, "retro_api_version") }?;
        // SAFETY: takes no arguments.
        let version = unsafe { api_version() };
        if version != RETRO_API_VERSION {
            return Err(load_error(
                core_path,
                format!("libretro API version {version}, not 1"),
            ));
        }
        // SAFETY: each `retro_*_fn` is its symbol's type, as above.
        let api = unsafe {
            Api {
                deinit: symbol(&library, core_path, "retro_deinit")?,
                reset: symbol(&library, core_path, "retro_reset")?,
                run: symbol(&library, core_path, "retro_run")?,
                serialize_size: symbol(&library, core_path, "retro_serialize_size")?,
                serialize: symbol(&library, core_path, "retro_serialize")?,
                unserialize: symbol(&library, core_path, "retro_unserialize")?,
                unload_game: symbol(&library, core_path, "retro_unload_game")?,
                get_memory_data: symbol(&library, core_path, "retro_get_memory_data")?,
                get_memory_size: symbol(&library, core_path, "retro_get_memory_size")?,
            }
        };
        // SAFETY: each `retro_*_fn` is its symbol's type, as above.
        let setup = unsafe {
            Setup {
                set_environment: symbol(&library, core_path, "retro_set_environment")?,
                set_video_refresh: symbol(&library, core_path, "retro_set_video_refresh")?,
                set_audio_sample: symbol(&library, core_path, "retro_set_audio_sample")?,
                set_audio_sample_batch: symbol(
                    &library,
                    core_path,
                    "retro_set_audio_sample_batch",
                )?,
                set_input_poll: symbol(&library, core_path, "retro_set_input_poll")?,
                set_input_state: symbol(&library, core_path, "retro_set_input_state")?,
                init: symbol(&library, core_path, "retro_init")?,
                get_system_info: symbol(&library, core_path, "retro_get_system_info")?,
                get_system_av_info: symbol(&library, core_path, "retro_get_system_av_info")?,
                load_game: symbol(&library, core_path, "retro_load_game")?,
                set_controller_port_device: symbol(
                    &library,
                    core_path,
                    "retro_set_controller_port_device",
                )?,
            }
        };

        let mut info = retro_system_info {
            library_name: std::ptr::null(),
            library_version: std::ptr::null(),
            valid_extensions: std::ptr::null(),
            need_fullpath: false,
            block_extract: false,
        };
        // SAFETY: `info` is writable. The strings it gets are static in the core (libretro.h)
        // and copied before anything else is called.
        let system_info = unsafe {
            (setup.get_system_info)(&mut info);
            SystemInfo {
                library_name: core_string(info.library_name),
                library_version: core_string(info.library_version),
                valid_extensions: core_string(info.valid_extensions),
                need_fullpath: info.need_fullpath,
            }
        };

        let content = match content_path {
            None => None,
            Some(content_path) => {
                let path = path_cstring(content_path).ok_or_else(|| {
                    load_error(
                        core_path,
                        format!(
                            "content path {} isn't UTF-8 without NULs",
                            content_path.display()
                        ),
                    )
                })?;
                let data = if system_info.need_fullpath {
                    Vec::new()
                } else {
                    std::fs::read(content_path).map_err(|error| Error::Content {
                        core: core_path.to_owned(),
                        path: content_path.to_owned(),
                        reason: format!(
                            "{error} (the core reads content into memory, need_fullpath is \
                             false, so the file must exist)"
                        ),
                    })?
                };
                Some((path, data))
            }
        };

        let mut core = Core {
            path: core_path.to_owned(),
            api,
            system_info,
            av_info: AvInfo {
                base_width: 0,
                base_height: 0,
                max_width: 0,
                max_height: 0,
                aspect_ratio: 0.0,
                fps: 0.0,
                sample_rate: 0.0,
            },
            frames_run: 0,
            settle_frames: 0,
            initialized: false,
            game_loaded: false,
            load_state: None,
            areas_found: HashMap::new(),
            _content: content,
            save_dir,
            _library: library,
            _claim: claim,
        };

        // SAFETY: the callbacks are 'static functions of the libretro types, valid for as long
        // as the library is loaded. libretro.h orders these calls: `retro_set_environment`
        // first, then the other callbacks, then `retro_init`.
        unsafe {
            (setup.set_environment)(Some(host::environment));
            (setup.set_video_refresh)(Some(host::video_refresh));
            (setup.set_audio_sample)(Some(host::audio_sample));
            (setup.set_audio_sample_batch)(Some(host::audio_sample_batch));
            (setup.set_input_poll)(Some(host::input_poll));
            (setup.set_input_state)(Some(host::input_state));
            (setup.init)();
        }
        core.initialized = true;
        Ok((core, setup))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn system_info(&self) -> &SystemInfo {
        &self.system_info
    }

    pub fn av_info(&self) -> AvInfo {
        self.av_info
    }

    /// The temporary directory the core saves into (`GET_SAVE_DIRECTORY`).
    pub fn save_dir(&self) -> &Path {
        &self.save_dir.0
    }

    /// Whether [`reset`](Self::reset) restores the state right after loading (`true`) or
    /// falls back to `retro_reset` (`false`, when the core couldn't serialize at load time or
    /// refused that state on a reset; once false, it stays false).
    pub fn resets_to_load_state(&self) -> bool {
        self.load_state.is_some()
    }

    /// Neutral frames run after each savestate load ([`load`](Self::load)).
    pub fn settle_frames(&self) -> u32 {
        self.settle_frames
    }

    /// `retro_run` calls since loading (resets and savestates don't change it).
    pub fn frames_run(&self) -> u64 {
        self.frames_run
    }

    /// Audio frames the core produced since loading. Headless, they are counted and dropped;
    /// see [`set_audio_capture`](Self::set_audio_capture) to play them.
    pub fn audio_frames(&self) -> u64 {
        host::host().audio_frames
    }

    /// Whether to keep the core's audio samples for [`take_audio`](Self::take_audio) (a
    /// player that plays them) or drop them (headless, the state after loading). Turning
    /// capture on or off discards the samples kept so far.
    pub fn set_audio_capture(&mut self, capture: bool) {
        host::host().audio = capture.then(Vec::new);
    }

    /// Appends the samples the core produced since the last call to `out`, interleaved
    /// stereo at [`AvInfo::sample_rate`], and forgets them. Appends nothing unless capture is
    /// on ([`set_audio_capture`](Self::set_audio_capture)).
    pub fn take_audio(&mut self, out: &mut Vec<i16>) {
        if let Some(audio) = &mut host::host().audio {
            out.extend_from_slice(audio);
            audio.clear();
        }
    }

    /// Runs `frames` frames with port 0's joypad `buttons` (bit n = button id n) held for all
    /// of them. This is the frame skip: only the last frame is kept, read with
    /// [`with_frame`](Self::with_frame). `frames == 0` runs nothing.
    pub fn run_frames(&mut self, frames: u32, buttons: u16) {
        host::host().buttons = buttons;
        for _ in 0..frames {
            // SAFETY: the game is loaded, and `&mut self` makes this the only call into the
            // core. The host isn't locked here, so the callbacks can lock it.
            unsafe { (self.api.run)() };
        }
        self.frames_run += u64::from(frames);
    }

    /// Runs each `(buttons, frames)` pair in order, as [`run_frames`](Self::run_frames) does:
    /// a scripted input sequence, such as a game's reset script.
    pub fn run_inputs(&mut self, inputs: &[(u16, u32)]) {
        for &(buttons, frames) in inputs {
            self.run_frames(frames, buttons);
        }
    }

    /// Calls `f` with the last frame the core drew, rows packed. Black, at the base size,
    /// after loading, a reset or loading a savestate, until the core draws a frame: a
    /// savestate holds no frame.
    pub fn with_frame<R>(&self, f: impl FnOnce(FrameRef<'_>) -> R) -> R {
        let host = host::host();
        f(FrameRef {
            data: &host.frame,
            width: host.width,
            height: host.height,
            pitch: host.width * host.format.bytes_per_pixel(),
            format: host.format,
        })
    }

    fn clear_frame(&mut self) {
        let (width, height) = (self.av_info.base_width, self.av_info.base_height);
        host::host().clear_frame(width as usize, height as usize);
    }

    /// Returns to the state right after loading by restoring the savestate taken then, so
    /// episodes start identically. `retro_reset` is the console's reset button, which in many
    /// cores keeps RAM or NVRAM, so it is only the fallback for cores that couldn't serialize
    /// at load time (some report no savestate size before their first frame), or that refuse
    /// their own state ([`resets_to_load_state`](Self::resets_to_load_state)).
    pub fn reset(&mut self) {
        // A refused snapshot stays dropped, so `resets_to_load_state` tells the truth.
        if let Some(state) = self.load_state.take()
            && self.unserialize(&state).is_ok()
        {
            self.load_state = Some(state);
            return;
        }
        // SAFETY: the game is loaded; `&mut self` makes this the only call into the core.
        unsafe { (self.api.reset)() };
        self.clear_frame();
    }

    /// The core's savestate. Fails with [`Error::Savestate`] if the core has none.
    pub fn serialize(&mut self) -> Result<Vec<u8>> {
        // SAFETY: the game is loaded; `&mut self` makes this the only call into the core.
        let size = unsafe { (self.api.serialize_size)() };
        if size == 0 {
            return Err(self.savestate_error("the core has no savestates"));
        }
        let mut state = vec![0; size];
        // SAFETY: `state` is writable for `size` bytes.
        if !unsafe { (self.api.serialize)(state.as_mut_ptr().cast(), size) } {
            return Err(self.savestate_error("retro_serialize failed"));
        }
        Ok(state)
    }

    /// Loads a savestate from [`serialize`](Self::serialize), settling as
    /// [`load`](Self::load) describes. Fails with [`Error::Savestate`] if the core refuses
    /// it.
    pub fn unserialize(&mut self, state: &[u8]) -> Result<()> {
        if !self.settle(state) {
            return Err(self.savestate_error("the core refused the state"));
        }
        self.clear_frame();
        Ok(())
    }

    /// Loads `state`, then, with `settle_frames`, runs that many neutral frames unseen and
    /// loads it again. `false` if the core refused it.
    fn settle(&mut self, state: &[u8]) -> bool {
        if !self.load_raw(state) {
            return false;
        }
        if self.settle_frames == 0 {
            return true;
        }
        let (buttons, audio_frames, audio_len) = {
            let mut host = host::host();
            let kept = (
                host.buttons,
                host.audio_frames,
                host.audio.as_ref().map(Vec::len),
            );
            host.buttons = 0;
            kept
        };
        for _ in 0..self.settle_frames {
            // SAFETY: as in `run_frames`.
            unsafe { (self.api.run)() };
        }
        {
            let mut host = host::host();
            host.buttons = buttons;
            host.audio_frames = audio_frames;
            if let (Some(audio), Some(len)) = (&mut host.audio, audio_len) {
                audio.truncate(len);
            }
        }
        self.load_raw(state)
    }

    /// `retro_unserialize`.
    fn load_raw(&mut self, state: &[u8]) -> bool {
        // SAFETY: `state` is readable for its length; `&mut self` makes this the only call
        // into the core.
        unsafe { (self.api.unserialize)(state.as_ptr().cast(), state.len()) }
    }

    fn savestate_error(&self, reason: &str) -> Error {
        Error::Savestate {
            core: self.system_info.library_name.clone(),
            reason: reason.to_owned(),
        }
    }

    /// The core's memory `id` (`RETRO_MEMORY_*`), or `None` if it has none. FBNeo exposes
    /// arcade RAM only as `RETRO_MEMORY_SYSTEM_RAM`.
    pub fn memory(&mut self, id: u32) -> Option<&[u8]> {
        // SAFETY: plain integers; `&mut self` makes these the only calls into the core.
        let (data, size) = unsafe {
            (
                (self.api.get_memory_data)(id),
                (self.api.get_memory_size)(id),
            )
        };
        if data.is_null() || size == 0 {
            return None;
        }
        // SAFETY: the core's memory `id` is `size` bytes at `data` until the game is unloaded,
        // and only changes inside calls into the core, which need `&mut self` while the slice
        // borrows it.
        Some(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) })
    }

    /// Whether the core gave the harness's named-area lookup (docs/libretro.md, "Named areas").
    /// Our FBNeo fork does; stock cores don't.
    pub fn has_areas(&self) -> bool {
        host::host().get_area.is_some()
    }

    /// The core's state area `name`, or `None` if the core has no area lookup, no such area,
    /// or several with that name. See [`areas`](Self::areas).
    pub fn area(&mut self, name: &str) -> Option<&[u8]> {
        self.areas([name]).map(|[area]| area)
    }

    /// The core's state areas `names`, or `None` if any is missing. Each name is looked up
    /// once per load and its address cached: use only areas whose storage is fixed while the
    /// game is loaded (allocated at init or static), not a bank pointer a driver moves.
    pub fn areas<const N: usize>(&mut self, names: [&str; N]) -> Option<[&[u8]; N]> {
        let mut found = [(0usize, 0usize); N];
        for (slot, name) in found.iter_mut().zip(names) {
            *slot = match self.areas_found.get(name) {
                Some(area) => (*area)?,
                None => {
                    let area = self.find_area(name);
                    self.areas_found.insert(name.to_owned(), area);
                    area?
                }
            };
        }
        // SAFETY: each area is `len` bytes at `address` until the game is unloaded (the core's
        // contract for `get_area`, and the caller's choice of areas), and changes only inside
        // calls into the core, which need `&mut self` while the slices borrow it.
        Some(
            found.map(|(address, len)| unsafe {
                std::slice::from_raw_parts(address as *const u8, len)
            }),
        )
    }

    /// Asks the core for area `name`: its address and length.
    fn find_area(&mut self, name: &str) -> Option<(usize, usize)> {
        let get_area = host::host().get_area?;
        let name = CString::new(name).ok()?;
        let (mut data, mut size) = (std::ptr::null(), 0usize);
        // SAFETY: a C string and two out-pointers, valid for the call; `&mut self` makes this
        // the only call into the core. The host lock is released, so the core may call back.
        let found = unsafe { get_area(name.as_ptr(), &mut data, &mut size) };
        (found && !data.is_null() && size > 0).then_some((data as usize, size))
    }

    /// The memory map regions the core registered (`SET_MEMORY_MAPS`); empty if none.
    pub fn memory_regions(&self) -> Vec<MemoryRegion> {
        host::host().regions.clone()
    }

    /// `len` bytes at emulated `address`, if one memory map region holds all of them. Regions
    /// are read as contiguous from `start`; mirrors (`select`, `disconnect`) aren't resolved.
    pub fn read_mapped(&mut self, address: usize, len: usize) -> Option<&[u8]> {
        let region = host::host().regions.iter().copied().find(|region| {
            address >= region.start
                && address
                    .checked_add(len)
                    .zip(region.start.checked_add(region.len))
                    .is_some_and(|(end, region_end)| end <= region_end)
        })?;
        let ptr = (region.host_address + (address - region.start)) as *const u8;
        // SAFETY: the core registered `region.len` bytes at `host_address` that live until the
        // game is unloaded, and the range is inside them. They change only inside calls into
        // the core, which need `&mut self` while the slice borrows it.
        Some(unsafe { std::slice::from_raw_parts(ptr, len) })
    }

    /// RAM variable `name`'s bytes, or [`Error::Ram`] if they aren't in the core's memory.
    fn ram_bytes(&mut self, name: &str, var: &RamVar) -> Result<&[u8]> {
        let error = |reason: String| Error::Ram {
            name: name.to_owned(),
            reason,
        };
        Ok(match var.location {
            Location::Memory { memory, offset } => {
                let data = self
                    .memory(memory.id())
                    .ok_or_else(|| error(format!("the core exposes no {memory:?}")))?;
                offset
                    .checked_add(var.size)
                    .and_then(|end| data.get(offset..end))
                    .ok_or_else(|| {
                        error(format!(
                            "{} bytes at {offset:#x} are past the end of {memory:?} ({} bytes)",
                            var.size,
                            data.len()
                        ))
                    })?
            }
            Location::Address(address) => self.read_mapped(address, var.size).ok_or_else(|| {
                error(format!(
                    "{} bytes at {address:#x} aren't in one memory map region",
                    var.size
                ))
            })?,
        })
    }

    /// Reads and decodes RAM variable `name`. Fails with [`Error::Ram`] if its bytes aren't
    /// in the core's memory or don't decode (docs/games.md, "RAM variables").
    pub fn read_ram(&mut self, name: &str, var: &RamVar) -> Result<i64> {
        let bytes = self.ram_bytes(name, var)?;
        var.decode(bytes).map_err(|reason| Error::Ram {
            name: name.to_owned(),
            reason,
        })
    }

    /// Checks that every variable in `vars` lies in the core's memory, so that reading them
    /// later can fail only on bytes that don't decode (a BCD nibble above 9). Memory sizes
    /// and maps are fixed once the game is loaded, so call this once after loading.
    pub fn check_ram_vars(&mut self, vars: &BTreeMap<String, RamVar>) -> Result<()> {
        vars.iter()
            .try_for_each(|(name, var)| self.ram_bytes(name, var).map(|_| ()))
    }

    /// Reads every variable in `vars` ([`read_ram`](Self::read_ram)), by name.
    pub fn read_ram_vars(
        &mut self,
        vars: &BTreeMap<String, RamVar>,
    ) -> Result<BTreeMap<String, i64>> {
        vars.iter()
            .map(|(name, var)| Ok((name.clone(), self.read_ram(name, var)?)))
            .collect()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // SAFETY: libretro.h's teardown order, each call made at most once and only after the
        // matching load. The library and claim fields drop after this.
        unsafe {
            if self.game_loaded {
                (self.api.unload_game)();
            }
            if self.initialized {
                (self.api.deinit)();
            }
        }
    }
}
