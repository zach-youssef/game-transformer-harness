//! gth-play on the test core (docs/libretro.md, "Test core"): the headless smoke replays
//! without desync, the session's hotkeys keep recordings whole, and the latency probe sees a
//! press on the frame it is pressed.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use gth_config::{CoreLock, CoresLock, GameConfig, RetroButton};
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
        game().settle_frames,
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

fn session(out: &Path, core_lock: Option<CoreLock>) -> Session {
    let settings = RecordSettings {
        dir: out.to_owned(),
        recorder: RecorderKind::Human,
        player: "test".into(),
        license: "CC0-1.0".into(),
        checks_every: NonZeroU32::new(7).unwrap(),
        core_lock,
        rewind_frames: 0,
    };
    Session::new(load(), game(), "testcore", None, settings)
}

#[test]
fn session_hotkeys_keep_recordings_whole() {
    let _serial = serial();
    let out = TempDir::new();
    let mut session = session(&out.0, None);
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

/// A `cores.lock` pinning the test core as `gth_testcore`, with this platform's hash taken
/// from the build at `core` (as the cores workflow writes a release's lock), written to
/// `<dir>/cores.lock`.
fn write_lock(dir: &Path, core: &Path) -> PathBuf {
    let unfilled = CoresLock::parse(
        "[cores.gth_testcore]\n\
         repo = \"https://example.invalid/gth-testcore\"\n\
         commit = \"0123456789abcdef0123456789abcdef01234567\"\n\
         library_version = \"test\"\n\
         [cores.gth_testcore.artifacts]\n",
        "test lock",
    )
    .unwrap();
    let target = gth_build_info::build_info()["target"];
    let builds = dir.join("builds");
    std::fs::create_dir_all(builds.join(target)).unwrap();
    let name = gth_config::library_file("gth_testcore", target);
    std::fs::copy(core, builds.join(target).join(name)).unwrap();
    let lock = unfilled.with_builds(&builds, &[target]).unwrap();
    let path = dir.join("cores.lock");
    std::fs::write(&path, lock.to_toml()).unwrap();
    path
}

/// The `[core.lock]` of a one-frame recording made with `core_lock`.
fn record_one(out: &Path, core_lock: Option<CoreLock>) -> Option<toml::Table> {
    let mut session = session(out, core_lock);
    let Event::RecordingStarted(dir) = session.toggle_recording().unwrap() else {
        panic!("started");
    };
    session.run_frame(0).unwrap();
    session.close().unwrap();
    Recording::open(&dir).unwrap().meta.core.lock
}

#[test]
fn recordings_keep_the_pin_of_a_pinned_core() {
    let _serial = serial();
    let out = TempDir::new();
    let lock_path = write_lock(&out.0, gth_test_support::testcore_path());
    let pinned =
        gth_play::game::pinned_core(&lock_path, &game(), gth_test_support::testcore_path())
            .unwrap()
            .expect("the test core is the build the lock pins");
    let lock = record_one(&out.0.join("pinned"), Some(pinned)).expect("[core.lock]");
    assert_eq!(
        lock["commit"].as_str(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(
        lock["repo"].as_str(),
        Some("https://example.invalid/gth-testcore")
    );

    // Any other file is unpinned, and its recordings keep no [core.lock].
    let other = out.0.join("other_core");
    let mut bytes = std::fs::read(gth_test_support::testcore_path()).unwrap();
    bytes.push(0);
    std::fs::write(&other, bytes).unwrap();
    let unpinned = gth_play::game::pinned_core(&lock_path, &game(), &other).unwrap();
    assert_eq!(unpinned, None);
    assert_eq!(record_one(&out.0.join("unpinned"), unpinned), None);

    // So is a core the lock doesn't name.
    let mut renamed = game();
    renamed.core = "fbneo".into();
    let path = gth_test_support::testcore_path();
    assert_eq!(
        gth_play::game::pinned_core(&lock_path, &renamed, path),
        Ok(None)
    );
    // The lock is named by the caller, so a missing one is an error, not "unpinned".
    let error = gth_play::game::pinned_core(&out.0.join("missing.lock"), &game(), path);
    assert!(error.unwrap_err().contains("missing.lock"));

    // And playing needs one (and a rewind length): the binary refuses before opening a
    // window.
    let output = Command::new(env!("CARGO_BIN_EXE_gth-play"))
        .args(["testcore", "--games-dir"])
        .arg(games_dir())
        .args(["--data-dir"])
        .arg(&out.0)
        .args(["--out"])
        .arg(&out.0)
        .args([
            "--player",
            "test",
            "--license",
            "CC0-1.0",
            "--checks-every",
            "60",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--rewind-seconds is required"), "{stderr}");
    let output = Command::new(env!("CARGO_BIN_EXE_gth-play"))
        .args(["testcore", "--games-dir"])
        .arg(games_dir())
        .args(["--data-dir"])
        .arg(&out.0)
        .args(["--out"])
        .arg(&out.0)
        .args([
            "--player",
            "test",
            "--license",
            "CC0-1.0",
            "--checks-every",
            "60",
            "--rewind-seconds",
            "3",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--lock is required"), "{stderr}");
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

/// `--rewind-seconds` past the ring's cap is refused before a window opens (the bounds
/// themselves are the binary's unit test).
#[test]
fn a_rewind_length_past_the_cap_is_refused() {
    let out = TempDir::new();
    let output = Command::new(env!("CARGO_BIN_EXE_gth-play"))
        .args(["testcore", "--games-dir"])
        .arg(games_dir())
        .args(["--data-dir"])
        .arg(&out.0)
        .args(["--out"])
        .arg(&out.0)
        .args([
            "--player",
            "test",
            "--license",
            "CC0-1.0",
            "--checks-every",
            "60",
            "--rewind-seconds",
            "1e17",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("from 0 to 600"), "{stderr}");
}

/// `--record-from-reset`: whatever was played before, the recording starts at the state a
/// fresh core has after the game's reset (what `gth reference-scores` checks), paused.
#[test]
fn a_recording_from_the_reset_starts_where_an_episode_does() {
    let _serial = serial();
    let out = TempDir::new();
    let mut session = session(&out.0, None);
    for _ in 0..10 {
        session.run_frame(RetroButton::A.bit()).unwrap();
    }
    assert_eq!(
        gth_play::game::reset_state(&game(), "testcore", &out.0).unwrap(),
        None,
        "a script reset reads no state"
    );
    let error = session.record_from_reset(Some(b"state")).unwrap_err();
    assert!(error.contains("resets by script"), "{error}");
    assert!(session.recording().is_none());

    let events = session.record_from_reset(None).unwrap();
    let [Event::RecordingStarted(dir), Event::Paused(true)] = events.as_slice() else {
        panic!("{events:?}");
    };
    let dir = dir.clone();
    assert!(!session.run_frame(0).unwrap(), "paused at the reset");
    let error = session.record_from_reset(None).unwrap_err();
    assert!(error.contains("a recording is running"), "{error}");
    session.toggle_pause();
    for _ in 0..5 {
        session.run_frame(RetroButton::A.bit()).unwrap();
    }
    session.close().unwrap();
    drop(session);

    let mut fresh = load();
    fresh.reset();
    fresh.run_inputs(&game().reset_inputs().unwrap());
    let expected = fresh.serialize().unwrap();
    drop(fresh);
    let recording = Recording::open(&dir).unwrap();
    assert_eq!(recording.start_state, expected);
    assert_eq!(recording.meta.frames, 5);
    let mut replayer = Replayer::new(recording, load()).unwrap();
    assert_eq!(replayer.run_to_end().unwrap(), 5);
}
