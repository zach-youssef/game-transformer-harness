//! The window: winit events, frame pacing and softbuffer presents around a [`Session`].

use std::collections::HashSet;
use std::io::Write;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gth_play::bindings::{self, KeyboardBindings};
use gth_play::display::{Screen, integer_scale};
use gth_play::input::clean_opposites;
use gth_play::latency;
use gth_play::session::{Event, Session};
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::pads::Pads;
use crate::sound::Sound;

/// Plays until the window closes, the quit hotkey, Cmd+Q (macOS) or Ctrl+C in the terminal;
/// finishes any recording on every one of them. `quit_after`: quit after that many frames
/// (unattended runs and timing checks).
pub fn run(
    session: Session,
    screen: Screen,
    game_id: &str,
    quit_after: Option<u64>,
) -> Result<(), String> {
    let dir = bindings::bindings_dir()
        .ok_or("no config directory: set GTH_CONFIG, XDG_CONFIG_HOME or HOME (APPDATA)")?;
    let keyboard = bindings::load_or_create(
        &dir.join("keyboard.toml"),
        bindings::KEYBOARD_TEMPLATE,
        bindings::parse_keyboard,
    )?;
    eprintln!(
        "gth-play: keyboard bindings in {}",
        dir.join("keyboard.toml").display()
    );
    let av = session.core().av_info();
    if !av.fps.is_finite() || av.fps <= 0.0 {
        return Err(format!("the core reports {} frames a second", av.fps));
    }
    // Ctrl+C, SIGTERM and SIGHUP (closing the terminal; on Windows the console's Ctrl+C,
    // close, logoff and shutdown events: ctrlc's `termination` feature) would kill the
    // process with the recording unfinished. The handler only raises a flag, and the loop,
    // which wakes every frame, exits through `exiting`. A second signal exits at once, in
    // case the loop is stuck (a core hung in a frame).
    let interrupt = Arc::new(AtomicBool::new(false));
    let flag = interrupt.clone();
    let handler = move || {
        if flag.swap(true, Ordering::Relaxed) {
            std::process::exit(130);
        }
    };
    if let Err(error) = ctrlc::set_handler(handler) {
        eprintln!(
            "gth-play: Ctrl+C would lose a running recording ({error}); quit with the hotkey"
        );
    }
    let event_loop = EventLoop::new().map_err(|e| e.to_string())?;
    let pads = Pads::new(dir);
    let sound = Sound::new(av.sample_rate);
    let mut app = App::new(
        format!("gth-play: {game_id}"),
        session,
        screen,
        keyboard,
        pads,
        sound,
        Duration::from_secs_f64(1.0 / av.fps),
        quit_after,
        interrupt,
    );
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    // `exiting` has finished already; this is a no-op unless the loop ended without it.
    app.finish();
    app.error.take().map_or(Ok(()), Err)
}

struct Win {
    window: Rc<Window>,
    surface: Surface<Rc<Window>, Rc<Window>>,
}

struct App {
    title: String,
    session: Session,
    screen: Screen,
    keyboard: KeyboardBindings,
    pads: Option<Pads>,
    sound: Option<Sound>,
    /// One core frame at the core's rate.
    period: Duration,
    next_frame: Instant,
    window: Option<Win>,
    /// Keys held, for the keyboard's part of the mask.
    held: HashSet<KeyCode>,
    /// When the first input event since the last frame arrived.
    input_at: Option<Instant>,
    /// That time, for a frame whose mask it changed, until the frame is presented.
    shown_input_at: Option<Instant>,
    /// When the frame waiting to be presented ran, until it is.
    ran_at: Option<Instant>,
    /// Frame run to present, per frame: the player's own share of the latency.
    present_ms: Vec<f64>,
    /// Frames run.
    frames: u64,
    quit_after: Option<u64>,
    last_mask: u16,
    /// Input event to present, per input that changed the mask.
    latency_ms: Vec<f64>,
    /// Frames run more than a frame late (the window stalled).
    late_frames: u64,
    audio: Vec<i16>,
    /// A fatal error, returned once the loop exits.
    error: Option<String>,
    /// Raised by the Ctrl+C handler.
    interrupt: Arc<AtomicBool>,
    /// The recording is closed and the stats printed (`finish` runs once).
    finished: bool,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    fn new(
        title: String,
        mut session: Session,
        screen: Screen,
        keyboard: KeyboardBindings,
        pads: Option<Pads>,
        sound: Option<Sound>,
        period: Duration,
        quit_after: Option<u64>,
        interrupt: Arc<AtomicBool>,
    ) -> App {
        // Samples are kept only when there is a device to play them.
        session.core_mut().set_audio_capture(sound.is_some());
        App {
            title,
            session,
            screen,
            keyboard,
            pads,
            sound,
            period,
            next_frame: Instant::now(),
            window: None,
            held: HashSet::new(),
            input_at: None,
            shown_input_at: None,
            ran_at: None,
            present_ms: Vec::new(),
            frames: 0,
            quit_after,
            last_mask: 0,
            latency_ms: Vec::new(),
            late_frames: 0,
            audio: Vec::new(),
            error: None,
            interrupt,
            finished: false,
        }
    }

    /// Whether the loop should end: a fatal error, `--quit-after` reached, or Ctrl+C.
    fn should_exit(&self) -> bool {
        self.error.is_some()
            || self.quit_after.is_some_and(|n| self.frames >= n)
            || self.interrupt.load(Ordering::Relaxed)
    }

    fn mask(&self) -> u16 {
        let keys = self.keyboard.mask(|key| self.held.contains(&key));
        let pads = self.pads.as_ref().map_or(0, Pads::mask);
        clean_opposites(keys | pads)
    }

    /// Runs one core frame with the current input, queues its audio and asks for a redraw.
    fn tick(&mut self) {
        if let Some(pads) = &mut self.pads
            && pads.poll()
        {
            self.input_at.get_or_insert_with(Instant::now);
        }
        let mask = self.mask();
        let input_at = self.input_at.take();
        let ran_at = Instant::now();
        match self.session.run_frame(mask) {
            Ok(true) => {
                if mask != self.last_mask {
                    self.shown_input_at = input_at;
                }
                self.last_mask = mask;
                self.frames += 1;
                self.ran_at.get_or_insert(ran_at);
                self.audio.clear();
                self.session.core_mut().take_audio(&mut self.audio);
                if let Some(sound) = &mut self.sound {
                    sound.push(&self.audio);
                }
                if let Some(win) = &self.window {
                    win.window.request_redraw();
                }
            }
            Ok(false) => {}
            Err(error) => {
                eprintln!("gth-play: {error}");
                self.update_title();
            }
        }
    }

    fn present(&mut self) {
        let Some(win) = &mut self.window else {
            return;
        };
        let size = win.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        if let Err(error) = self.session.core().with_frame(|f| self.screen.convert(f)) {
            eprintln!("gth-play: cannot show the frame: {error}");
            return;
        }
        let result = win.surface.resize(w, h).and_then(|()| {
            let mut buffer = win.surface.buffer_mut()?;
            self.screen
                .draw(&mut buffer, w.get() as usize, h.get() as usize);
            buffer.present()
        });
        if let Err(error) = result {
            self.error = Some(format!("cannot present: {error}"));
        }
        if let Some(at) = self.shown_input_at.take() {
            self.latency_ms.push(at.elapsed().as_secs_f64() * 1000.0);
        }
        if let Some(at) = self.ran_at.take() {
            self.present_ms.push(at.elapsed().as_secs_f64() * 1000.0);
        }
    }

    fn hotkey(&mut self, key: KeyCode, event_loop: &ActiveEventLoop) {
        let hotkeys = self.keyboard.hotkeys;
        let events = if key == hotkeys.pause {
            let event = self.session.toggle_pause();
            if let Some(sound) = &mut self.sound {
                sound.set_paused(self.session.is_paused());
            }
            Ok(vec![event])
        } else if key == hotkeys.save_state {
            self.session.save_state().map(|e| vec![e])
        } else if key == hotkeys.load_state {
            self.session.load_state()
        } else if key == hotkeys.record {
            self.session.toggle_recording().map(|e| vec![e])
        } else if key == hotkeys.quit {
            event_loop.exit();
            return;
        } else {
            return;
        };
        match events {
            Ok(events) => {
                for event in events {
                    eprintln!("gth-play: {}", describe(&event));
                }
            }
            Err(error) => eprintln!("gth-play: {error}"),
        }
        self.update_title();
        if let Some(win) = &self.window {
            win.window.request_redraw();
        }
    }

    fn update_title(&self) {
        let Some(win) = &self.window else {
            return;
        };
        let mut title = self.title.clone();
        if let Some((_, frames)) = self.session.recording() {
            title.push_str(&format!(" [REC {frames}]"));
        }
        if self.session.is_paused() {
            title.push_str(" [PAUSED]");
        }
        win.window.set_title(&title);
    }

    /// Finishes any recording and reports the latency samples, once: later calls do
    /// nothing.
    /// Writes to stderr ignore errors: this runs from `Drop` while unwinding, where an
    /// `eprintln!` panic (stderr closed) would abort.
    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        match self.session.close() {
            Ok(Some(dir)) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "gth-play: recording finished: {}",
                    dir.display()
                );
            }
            Ok(None) => {}
            Err(error) => {
                let _ = writeln!(std::io::stderr(), "gth-play: {error}");
                self.error.get_or_insert(error);
            }
        }
        if let Some(s) = latency::summarize(&mut self.latency_ms) {
            let _ = writeln!(
                std::io::stderr(),
                "gth-play: input event to present: p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms \
                 over {} inputs (docs/human-play.md, \"Input latency\")",
                s.p50,
                s.p95,
                s.max,
                s.samples
            );
        }
        if let Some(s) = latency::summarize(&mut self.present_ms) {
            let _ = writeln!(
                std::io::stderr(),
                "gth-play: frame run to present: p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms \
                 over {} frames",
                s.p50,
                s.p95,
                s.max,
                s.samples
            );
        }
        let underruns = self.sound.as_ref().map_or(0, Sound::underruns);
        let _ = writeln!(
            std::io::stderr(),
            "gth-play: {} frames, {} late, {underruns} audio frames underrun",
            self.frames,
            self.late_frames
        );
    }
}

fn describe(event: &Event) -> String {
    match event {
        Event::Paused(true) => "paused".into(),
        Event::Paused(false) => "resumed".into(),
        Event::StateSaved(path) => format!("state saved ({})", path.display()),
        Event::StateLoaded => "state loaded".into(),
        Event::RecordingStarted(dir) => format!("recording to {}", dir.display()),
        Event::RecordingFinished(dir) => format!("recording finished: {}", dir.display()),
    }
}

/// A panic unwinding out of the loop still finishes the recording.
impl Drop for App {
    fn drop(&mut self) {
        self.finish();
    }
}

impl ApplicationHandler for App {
    /// The recording is closed here, not after `run_app` returns: on macOS, Cmd+Q (winit's
    /// default app menu) terminates the process through AppKit right after this call, so
    /// `run_app` never returns. Every other exit passes through here too.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.finish();
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let av = self.session.core().av_info();
        let (w, h) = self
            .screen
            .shown_size(av.base_width as usize, av.base_height as usize);
        // The largest whole scale that fits 90% of the screen.
        let scale = event_loop
            .primary_monitor()
            .or_else(|| event_loop.available_monitors().next())
            .map_or(2, |m| {
                let size = m.size();
                integer_scale(
                    w,
                    h,
                    size.width as usize * 9 / 10,
                    size.height as usize * 9 / 10,
                )
            });
        let attributes = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(PhysicalSize::new((w * scale) as u32, (h * scale) as u32));
        let created = event_loop
            .create_window(attributes)
            .map_err(|e| e.to_string())
            .and_then(|window| {
                let window = Rc::new(window);
                let context = Context::new(window.clone()).map_err(|e| e.to_string())?;
                let surface = Surface::new(&context, window.clone()).map_err(|e| e.to_string())?;
                Ok(Win { window, surface })
            });
        match created {
            Ok(win) => {
                self.window = Some(win);
                self.next_frame = Instant::now();
            }
            Err(error) => {
                self.error = Some(format!("cannot open a window: {error}"));
                event_loop.exit();
            }
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::Init) {
            self.next_frame = Instant::now();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.present(),
            WindowEvent::Resized(_) => {
                if let Some(win) = &self.window {
                    win.window.request_redraw();
                }
            }
            WindowEvent::Focused(false) => {
                // Keys released elsewhere never reach us.
                self.held.clear();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(key) = event.physical_key else {
                    return;
                };
                let pressed = event.state == ElementState::Pressed;
                if pressed && self.keyboard.hotkeys.contains(key) {
                    if !event.repeat {
                        self.hotkey(key, event_loop);
                    }
                    return;
                }
                if event.repeat {
                    return;
                }
                let changed = if pressed {
                    self.held.insert(key)
                } else {
                    self.held.remove(&key)
                };
                if changed {
                    self.input_at.get_or_insert_with(Instant::now);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_frame {
            if now > self.next_frame + self.period {
                // Too far behind (the window stalled): don't run frames to catch up.
                self.late_frames += 1;
                self.next_frame = now;
            }
            self.tick();
            self.next_frame += self.period;
            if self.session.recording().is_some()
                && self.session.core().frames_run().is_multiple_of(60)
            {
                self.update_title();
            }
        }
        if self.should_exit() {
            event_loop.exit();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::path::{Path, PathBuf};

    use gth_config::RetroButton;
    use gth_core::Core;
    use gth_play::session::RecordSettings;
    use gth_record::{RecorderKind, Recording, Replayer};

    use super::*;

    fn games_dir() -> PathBuf {
        let manifest = std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo test sets it");
        Path::new(&manifest).join("../../games")
    }

    fn load() -> Core {
        let game = gth_play::game::load_config(&games_dir(), "testcore").unwrap();
        Core::load(
            gth_test_support::testcore_path(),
            None,
            &std::env::temp_dir(),
            &game.core_options,
        )
        .unwrap()
    }

    /// The exit hook (`exiting`, also run by `Drop` and after the loop) closes a running
    /// recording whole, once; Ctrl+C's flag ends the loop. The only test in this binary that
    /// loads a core, so it needs no lock.
    #[test]
    fn exit_hook_finishes_the_recording_once() {
        let out = std::env::temp_dir().join(format!("gth-play-app-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let game = gth_play::game::load_config(&games_dir(), "testcore").unwrap();
        let settings = RecordSettings {
            dir: out.clone(),
            recorder: RecorderKind::Human,
            player: "test".into(),
            license: "CC0-1.0".into(),
            checks_every: NonZeroU32::new(5).unwrap(),
            core_lock: None,
        };
        let session = Session::new(load(), game.clone(), "testcore", None, settings);
        let keyboard = bindings::parse_keyboard(bindings::KEYBOARD_TEMPLATE, "template").unwrap();
        let interrupt = Arc::new(AtomicBool::new(false));
        let mut app = App::new(
            "test".into(),
            session,
            Screen::new(&game),
            keyboard,
            None,
            None,
            Duration::from_millis(16),
            None,
            interrupt.clone(),
        );
        app.session.toggle_recording().unwrap();
        app.held.insert(KeyCode::KeyX);
        for _ in 0..12 {
            app.tick();
        }
        assert!(!app.should_exit());
        interrupt.store(true, Ordering::Relaxed);
        assert!(app.should_exit(), "Ctrl+C ends the loop");

        let dir = app.session.recording().unwrap().0.to_owned();
        app.finish();
        assert!(app.session.recording().is_none());
        let recording = Recording::open(&dir).expect("finished: meta.toml written");
        assert_eq!(recording.meta.frames, 12);
        assert!(
            recording
                .inputs
                .iter()
                .all(|i| i.buttons == RetroButton::A.bit())
        );
        app.finish();
        drop(app);
        let mut replayer = Replayer::new(Recording::open(&dir).unwrap(), load()).unwrap();
        assert_eq!(replayer.run_to_end().unwrap(), 12);
        let _ = std::fs::remove_dir_all(&out);
    }
}
