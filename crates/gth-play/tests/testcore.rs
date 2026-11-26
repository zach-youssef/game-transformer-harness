//! gth-play on the test core (docs/libretro.md, "Test core"): the headless smoke replays
//! without desync, the session's hotkeys keep recordings whole, and the latency probe sees a
//! press on the frame it is pressed.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use gth_config::{GameConfig, RetroButton};
use gth_core::Core;
use gth_play::session::{Event, RecordSettings, Session};
use gth_play::{latency, smoke};
use gth_record::{RecorderKind, Recording, Replayer};

/// One core per process: the tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A fresh temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("gth-play-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The package's directory, read at run time (a stale `env!` from another worktree's build
/// would point elsewhere: docs/testing.md).
fn games_dir() -> PathBuf {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo test sets it");
    Path::new(&manifest).join("../../games")
}

fn game() -> GameConfig {
    gth_play::game::load_config(&games_dir(), "testcore").unwrap()
}

fn load() -> Core {
    Core::load(
        gth_test_support::testcore_path(),
        None,
        &std::env::temp_dir(),
        &game().core_options,
    )
    .unwrap()
}

#[test]
fn headless_smoke_replays_without_desync() {
    let _serial = serial();
    let out = TempDir::new();
    let output = Command::new(env!("CARGO_BIN_EXE_gth-play"))
        .arg("--headless-smoke")
        .arg("--core")
        .arg(gth_test_support::testcore_path())
        .arg("--games-dir")
        .arg(games_dir())
        .arg("--out")
        .arg(&out.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dir = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    assert!(dir.starts_with(&out.0));
    let name = dir.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("testcore-") && name.ends_with("-0000.gthrec"),
        "{name}"
    );

    let recording = Recording::open(&dir).unwrap();
    assert_eq!(recording.meta.frames, u64::from(smoke::FRAMES));
    assert_eq!(
        recording.meta.steps,
        u64::from(smoke::FRAMES),
        "frameskip 1"
    );
    assert_eq!(recording.meta.recorder, RecorderKind::Scripted);
    assert_eq!(recording.meta.harness["version"], env!("CARGO_PKG_VERSION"));
    let masks: Vec<u16> = recording.inputs.iter().map(|i| i.buttons).collect();
    let expected: Vec<u16> = (0..smoke::FRAMES).map(smoke::smoke_mask).collect();
    assert_eq!(
        masks, expected,
        "no frame added by the mid-recording hotkeys"
    );
    assert!(
        expected
            .iter()
            .all(|m| m & 0x30 != 0x30 && m & 0xc0 != 0xc0),
        "opposite directions never stored"
    );
    let directions: std::collections::BTreeSet<u16> = expected.iter().map(|m| m & 0xf0).collect();
    assert_eq!(
        directions.len(),
        9,
        "all 8 directions and neutral: {directions:?}"
    );

    let mut replayer = Replayer::new(recording, load()).unwrap();
    assert_eq!(replayer.run_to_end().unwrap(), u64::from(smoke::FRAMES));

    // `--verify` does the same from the command line.
    let output = Command::new(env!("CARGO_BIN_EXE_gth-play"))
        .arg("--verify")
        .arg(&dir)
        .arg("--data-dir")
        .arg(&out.0)
        .arg("--core")
        .arg(gth_test_support::testcore_path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("600 frames replayed, no desync"),
        "{stdout}"
    );
}

fn session(out: &Path) -> Session {
    let settings = RecordSettings {
        dir: out.to_owned(),
        recorder: RecorderKind::Human,
        player: "test".into(),
        license: "CC0-1.0".into(),
        checks_every: NonZeroU32::new(7).unwrap(),
    };
    Session::new(load(), game(), "testcore", None, settings)
}

#[test]
fn session_hotkeys_keep_recordings_whole() {
    let _serial = serial();
    let out = TempDir::new();
    let mut session = session(&out.0);
    let a = RetroButton::A.bit();

    assert!(
        session
            .load_state()
            .unwrap_err()
            .contains("no state saved yet")
    );
    for _ in 0..10 {
        assert!(session.run_frame(RetroButton::Right.bit()).unwrap());
    }
    let Event::StateSaved(state_file) = session.save_state().unwrap() else {
        panic!("saved");
    };
    assert_eq!(state_file, out.0.join("testcore.state"));
    let Event::RecordingStarted(first) = session.toggle_recording().unwrap() else {
        panic!("started");
    };
    for _ in 0..20 {
        session.run_frame(a).unwrap();
    }
    assert_eq!(session.toggle_pause(), Event::Paused(true));
    assert!(!session.run_frame(a).unwrap(), "no frame while paused");
    assert_eq!(session.toggle_pause(), Event::Paused(false));
    session.run_frame(0).unwrap();
    assert_eq!(session.recording().unwrap().1, 21);
    // Loading a state finishes the recording first: it can't hold the jump.
    let events = session.load_state().unwrap();
    assert_eq!(
        events,
        [Event::RecordingFinished(first.clone()), Event::StateLoaded]
    );
    assert!(session.recording().is_none());
    let recording = Recording::open(&first).unwrap();
    assert_eq!(recording.meta.frames, 21);
    assert_eq!(recording.meta.recorder, RecorderKind::Human);
    assert_eq!(recording.meta.player, "test");
    assert_eq!(recording.inputs[0].buttons, a);
    assert_eq!(recording.inputs[20].buttons, 0);

    // A second recording from the loaded state gets the next number, and close finishes it.
    let Event::RecordingStarted(second) = session.toggle_recording().unwrap() else {
        panic!("started");
    };
    assert!(second.to_str().unwrap().ends_with("-0001.gthrec"));
    session.run_frame(a).unwrap();
    assert_eq!(session.close().unwrap(), Some(second.clone()));
    assert_eq!(session.close().unwrap(), None);
    // The slot is the state saved before the first recording, which is where both start.
    let start = Recording::open(&second).unwrap().start_state;
    assert_eq!(start, recording.start_state);
    assert_eq!(std::fs::read(&state_file).unwrap(), start);

    drop(session);
    for dir in [first, second] {
        let mut replayer = Replayer::new(Recording::open(&dir).unwrap(), load()).unwrap();
        replayer.run_to_end().unwrap();
    }
}

#[test]
fn testcore_shows_a_press_on_its_own_frame() {
    let _serial = serial();
    let mut core = load();
    core.run_frames(5, 0);
    let state = core.serialize().unwrap();
    for button in [RetroButton::A, RetroButton::Start, RetroButton::Up] {
        assert_eq!(
            latency::frames_to_response(&mut core, &state, button.bit(), 10).unwrap(),
            Some(1),
            "{button:?}"
        );
    }
    assert_eq!(
        latency::frames_to_response(&mut core, &state, 0, 10).unwrap(),
        None
    );
}
