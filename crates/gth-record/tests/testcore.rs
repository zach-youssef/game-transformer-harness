//! Recording and replaying the test core (docs/libretro.md, "Test core") as games.toml's
//! `testcore` game, with random inputs.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use gth_config::{GameConfig, MemoryId, RetroButton};
use gth_core::Core;
use gth_record::{
    Error, FORMAT_VERSION, Recorder, RecorderKind, Recording, RecordingInfo, Replayer,
};

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
        let path = std::env::temp_dir().join(format!("gth-record-test-{}-{n}", std::process::id()));
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

fn game() -> GameConfig {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/testcore/game.toml");
    GameConfig::load(&path).unwrap()
}

/// Writes content (it seeds the test core's PRNG, so RAM changes every frame) into `dir`.
fn content(dir: &Path) -> PathBuf {
    let path = dir.join("testcore.gthc");
    std::fs::write(&path, b"gth-record test content\n").unwrap();
    path
}

fn load(game: &GameConfig, content: &Path) -> Core {
    Core::load(
        gth_test_support::testcore_path(),
        Some(content),
        &std::env::temp_dir(),
        &game.core_options,
    )
    .unwrap()
}

fn info(checks_every: u32) -> RecordingInfo {
    RecordingInfo {
        game_id: "testcore".into(),
        recorder: RecorderKind::Scripted,
        player: "random".into(),
        license: "CC0-1.0".into(),
        harness: BTreeMap::from([("version".into(), "test".into())]),
        core_lock: None,
        checks_every: NonZeroU32::new(checks_every).unwrap(),
    }
}

/// xorshift64, so the random inputs are the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Records `frames` frames of random buttons, in steps of 1 to 6 frames, from the game's
/// start (its reset script); returns the recording's directory and the final RAM.
fn record(dir: &Path, frames: u64, checks_every: u32, rng: &mut Rng) -> (PathBuf, Vec<u8>) {
    let game = game();
    let mut core = load(&game, &content(dir));
    core.run_inputs(&game.reset_inputs().unwrap());
    let path = dir.join("rec.gthrec");
    let mut recorder = Recorder::create(
        &path,
        &mut core,
        &game,
        Some(&content(dir)),
        info(checks_every),
    )
    .unwrap();
    while recorder.frames() < frames {
        let step = (1 + rng.next() % 6).min(frames - recorder.frames()) as u32;
        // Any buttons, d-pad included: the recording is the RetroPad mask, not the action.
        let buttons = rng.next() as u16;
        recorder.run(&mut core, step, buttons).unwrap();
    }
    let path = recorder.finish(&mut core).unwrap();
    let ram = core.memory(MemoryId::SystemRam.id()).unwrap().to_vec();
    (path, ram)
}

fn replay(path: &Path, content: &Path) -> gth_record::Result<Replayer> {
    let game = game();
    Replayer::new(Recording::open(path)?, load(&game, content))
}

#[test]
fn five_thousand_random_frames_replay_without_desync() {
    let _serial = serial();
    let dir = TempDir::new();
    let (path, ram) = record(&dir.0, 5000, 60, &mut Rng(1));
    let recording = Recording::open(&path).unwrap();
    assert_eq!(recording.meta.frames, 5000);
    assert_eq!(recording.meta.format_version, FORMAT_VERSION);
    assert_eq!(recording.meta.checks_every, 60);
    // 83 every 60 frames plus one after frame 5000.
    assert_eq!(recording.checks.len(), 84);
    assert_eq!(
        recording.meta.content.as_ref().unwrap().file,
        "testcore.gthc"
    );
    assert_eq!(recording.meta.core.library_name, "gth-testcore");
    let steps = recording.meta.steps;

    let mut replayer = replay(&path, &content(&dir.0)).unwrap();
    let mut replayed_steps = 0;
    while let Some((_, frames)) = replayer.step().unwrap() {
        assert!((1..=6).contains(&frames));
        replayed_steps += 1;
    }
    assert_eq!((replayer.frame(), replayed_steps), (5000, steps));
    drop(replayer);
    let mut replayer = replay(&path, &content(&dir.0)).unwrap();
    assert_eq!(replayer.run_to_end().unwrap(), 5000);
    drop(replayer);
    // The replay ends in the recorded state, not just one that hashes the same at checks.
    let game = game();
    let mut core = load(&game, &content(&dir.0));
    let recording = Recording::open(&path).unwrap();
    core.unserialize(&recording.start_state).unwrap();
    for input in &recording.inputs {
        core.run_frames(1, input.buttons);
    }
    assert_eq!(core.memory(MemoryId::SystemRam.id()).unwrap(), ram);
}

/// Rewrites one frame's buttons in `inputs.bin` (frame 1-based), toggling A, which the test
/// core scores on.
fn tamper(path: &Path, frame: usize) {
    let file = path.join("inputs.bin");
    let mut data = zstd::decode_all(std::fs::read(&file).unwrap().as_slice()).unwrap();
    let at = 8 + 3 * (frame - 1);
    let buttons = u16::from_le_bytes([data[at], data[at + 1]]) ^ RetroButton::A.bit();
    data[at..at + 2].copy_from_slice(&buttons.to_le_bytes());
    std::fs::write(&file, zstd::encode_all(data.as_slice(), 3).unwrap()).unwrap();
}

#[test]
fn a_tampered_input_is_detected_at_its_frame() {
    let _serial = serial();
    // (checks every, tampered frame, first and last frame of the desync's range)
    let cases = [
        (1, 777, 777, 777),
        (60, 777, 721, 780),
        (60, 1990, 1981, 2000),
    ];
    for (checks_every, tampered, first_detected, detected) in cases {
        let dir = TempDir::new();
        let (path, _) = record(&dir.0, 2000, checks_every, &mut Rng(7));
        tamper(&path, tampered);
        let mut replayer = replay(&path, &content(&dir.0)).unwrap();
        match replayer.run_to_end() {
            Err(Error::Desync {
                first,
                frame,
                detail,
            }) => {
                let range = (first, frame);
                assert_eq!(
                    range,
                    (first_detected, detected),
                    "{checks_every}: {detail}"
                );
                assert!(detail.starts_with("RAM variable score"), "{detail}");
            }
            other => panic!("every {checks_every}: expected a desync, got {other:?}"),
        }
    }
}

#[test]
fn other_format_versions_and_inconsistent_files_are_refused() {
    let _serial = serial();
    let dir = TempDir::new();
    let (path, _) = record(&dir.0, 100, 60, &mut Rng(3));
    let meta = std::fs::read_to_string(path.join("meta.toml")).unwrap();
    let edit = |from: &str, to: &str| {
        assert!(meta.contains(from), "{from}");
        std::fs::write(path.join("meta.toml"), meta.replacen(from, to, 1)).unwrap();
    };
    let open_error = || Recording::open(&path).unwrap_err().to_string();

    edit("format_version = 1", "format_version = 999");
    let error = open_error();
    assert!(
        error.contains("format version 999; this build reads version 1"),
        "{error}"
    );
    // A future version's new fields don't hide the version.
    edit("format_version = 1", "format_version = 2\nnew_field = 1");
    assert!(open_error().contains("format version 2"));
    edit("frames = 100", "frames = 101");
    assert!(open_error().contains("inputs.bin has 100 frames"));
    edit("checks_every = 60", "checks_every = 50");
    assert!(open_error().contains("check frames"));
    edit("game_id", "unknown = 1\ngame_id");
    assert!(open_error().contains("unknown field"));

    edit("library_version = \"", "library_version = \"other ");
    let error = replay(&path, &content(&dir.0)).err().unwrap();
    assert!(matches!(error, Error::Mismatch { .. }), "{error}");
    assert!(
        error
            .to_string()
            .contains("recorded with core gth-testcore other"),
        "{error}"
    );

    std::fs::write(path.join("meta.toml"), &meta).unwrap();
    let game_toml = path.join("game.toml");
    let text = std::fs::read_to_string(&game_toml).unwrap();
    std::fs::write(&game_toml, text.replace("frames = 3", "frames = 4")).unwrap();
    assert!(open_error().contains("game.toml doesn't have the SHA-1"));
    std::fs::write(&game_toml, text).unwrap();

    let other = dir.0.join("other.gthc");
    std::fs::write(&other, b"other content").unwrap();
    let recording = Recording::open(&path).unwrap();
    let error = Replayer::load(
        recording,
        gth_test_support::testcore_path(),
        Some(&other),
        &std::env::temp_dir(),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("content"), "{error}");

    let recording = Recording::open(&path).unwrap();
    let error = Replayer::load(
        recording,
        gth_test_support::testcore_path(),
        None,
        &std::env::temp_dir(),
    )
    .err()
    .unwrap();
    assert!(matches!(error, Error::Mismatch { .. }), "{error}");
    assert!(error.to_string().contains("none was given"), "{error}");

    std::fs::remove_file(path.join("meta.toml")).unwrap();
    assert!(open_error().contains("incomplete"));
}

#[test]
fn a_core_that_repeats_its_frame_replays_the_same_frames() {
    let _serial = serial();
    let dir = TempDir::new();
    let mut game = game();
    // The test core sends no new frame while no button is held.
    game.core_options
        .insert("gth_testcore_dupe".into(), "on".into());
    let content = content(&dir.0);
    let mut core = load(&game, &content);
    // Ends on a frame with no button, which repeats the last one drawn (A held).
    core.run_inputs(&game.reset_inputs().unwrap());
    let path = dir.0.join("dupe.gthrec");
    let mut recorder = Recorder::create(&path, &mut core, &game, Some(&content), info(1)).unwrap();
    // Repeated frames, checked every frame, then drawn ones.
    recorder.run(&mut core, 3, 0).unwrap();
    recorder.run(&mut core, 3, RetroButton::A.bit()).unwrap();
    let path = recorder.finish(&mut core).unwrap();
    drop(core);
    let recording = Recording::open(&path).unwrap();
    assert_eq!(recording.game.core_options["gth_testcore_dupe"], "on");
    let mut replayer = Replayer::load(
        recording,
        gth_test_support::testcore_path(),
        Some(&content),
        &std::env::temp_dir(),
    )
    .unwrap();
    assert_eq!(replayer.run_to_end().unwrap(), 6);
}

#[test]
fn a_minute_at_60_fps_is_under_100_kb() {
    let _serial = serial();
    let dir = TempDir::new();
    let game = game();
    let mut core = load(&game, &content(&dir.0));
    let path = dir.0.join("minute.gthrec");
    let mut recorder = Recorder::create(&path, &mut core, &game, None, info(60)).unwrap();
    let mut rng = Rng(11);
    // A policy at frameskip 4: a random action every 4 frames, as RetroEnv records.
    let table = gth_config::ActionSpace::new(&game.buttons).joypad_table();
    for _ in 0..3600 / 4 {
        let action = (rng.next() % table.len() as u64) as usize;
        recorder.run(&mut core, 4, table[action]).unwrap();
    }
    let path = recorder.finish(&mut core).unwrap();
    let size: u64 = std::fs::read_dir(&path)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(size < 100_000, "{size} bytes");
    let recording = Recording::open(&path).unwrap();
    assert_eq!((recording.meta.frames, recording.meta.steps), (3600, 900));
    assert_eq!(recording.meta.content, None);
    assert_eq!(recording.meta.fps, 60.0);
}
