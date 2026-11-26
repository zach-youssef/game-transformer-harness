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

/// A shard's members, by name.
fn shard_members(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let data = std::fs::read(path).unwrap();
    let archive = rawzip::ZipArchive::from_slice(&data).unwrap();
    let mut members = BTreeMap::new();
    let mut entries = archive.entries();
    while let Some(entry) = entries.next_entry().unwrap() {
        assert_eq!(entry.compression_method(), rawzip::CompressionMethod::STORE);
        let name = String::from_utf8_lossy(entry.file_path().as_ref()).into_owned();
        let local = archive.get_entry(entry.wayfinder()).unwrap();
        members.insert(name, local.data().to_vec());
    }
    members
}

/// An `.npy` member's data, after its header.
fn npy_data(member: &[u8]) -> &[u8] {
    let header_len = u16::from_le_bytes([member[8], member[9]]) as usize;
    &member[10 + header_len..]
}

#[test]
fn a_recording_exports_to_a_shard_of_its_steps() {
    let _serial = serial();
    let dir = TempDir::new();
    let game = game();
    let space = gth_config::ActionSpace::new(&game.buttons);
    let table = space.joypad_table();
    let mut core = load(&game, &content(&dir.0));
    core.run_inputs(&game.reset_inputs().unwrap());
    let path = dir.0.join("export.gthrec");
    let mut recorder =
        Recorder::create(&path, &mut core, &game, Some(&content(&dir.0)), info(60)).unwrap();
    // Two idle steps, then random actions at frameskip 4, one of them with start held.
    let mut rng = Rng(3);
    let mut masks = vec![0, 0];
    masks.extend((0..60).map(|_| table[(rng.next() % table.len() as u64) as usize]));
    masks[30] |= RetroButton::Start.bit();
    for &mask in &masks {
        recorder.run(&mut core, 4, mask).unwrap();
    }
    recorder.finish(&mut core).unwrap();
    drop(core);

    let options = |steps| gth_record::ShardOptions {
        obs_height: 60,
        obs_width: 80,
        crop: Some(gth_frame::Rect {
            x: 0,
            y: 0,
            width: 320,
            height: 232,
        }),
        rotation: gth_frame::Rotation::Cw90,
        interpolation: gth_frame::Filter::Area,
        steps,
        chunk_samples: NonZeroU32::new(16).unwrap(),
        harness: BTreeMap::from([("version".into(), "test".into())]),
    };
    let out = dir.0.join("export.npz");
    let mut replayer = replay(&path, &content(&dir.0)).unwrap();
    let summary = gth_record::export(
        &mut replayer,
        &options(gth_record::Steps::Recorded),
        &mut gth_record::KeepAll,
        &out,
    )
    .unwrap();
    drop(replayer);
    // A sample per observation that has a following frame: steps 0-60 of 62. The frame after
    // step 29's observation holds start, so that sample has no label.
    assert_eq!(
        (
            summary.samples,
            summary.frames,
            summary.unlabelled,
            summary.filtered
        ),
        (60, 62 * 4, 1, 0)
    );
    assert_eq!(summary.frame_chunks, 4);
    assert_eq!(summary.bytes, std::fs::metadata(&out).unwrap().len());
    assert!(!dir.0.join("export.npz.partial").exists());

    let members = shard_members(&out);
    let meta: gth_record::ShardMeta = serde_json::from_slice(&members["meta.json"]).unwrap();
    assert_eq!(meta.shard_version, gth_record::SHARD_VERSION);
    assert_eq!(meta.source_id, "export.gthrec");
    assert_eq!(
        (
            meta.samples,
            meta.frames,
            meta.frame_chunks,
            meta.chunk_samples
        ),
        (60, 248, 4, 16)
    );
    assert_eq!(
        (meta.frameskip, meta.orientation, meta.obs_size),
        (Some(4), 90, [60, 80])
    );
    assert_eq!(meta.ram_names, game.ram.keys().cloned().collect::<Vec<_>>());
    assert_eq!(
        (meta.label_source.as_str(), meta.filter.as_str()),
        ("replay", "all")
    );

    // Sample j: step j's last frame, labelled with step j + 1's action (the one chosen after
    // seeing it), in two segments around the unlabelled sample.
    let observed: Vec<usize> = (0..61).filter(|&j| j != 29).collect();
    let direction = npy_data(&members["direction.npy"]);
    let buttons = npy_data(&members["buttons.npy"]);
    let episode_frame = npy_data(&members["episode_frame.npy"]);
    let segment = npy_data(&members["segment.npy"]);
    for (sample, &j) in observed.iter().enumerate() {
        let action = space.action_of(masks[j + 1]).unwrap();
        assert_eq!(direction[sample], action.direction as u8);
        assert_eq!(buttons[sample], action.buttons);
        let frame = u64::from_le_bytes(episode_frame[sample * 8..][..8].try_into().unwrap());
        assert_eq!(frame, (j as u64 + 1) * 4);
        let id = u32::from_le_bytes(segment[sample * 4..][..4].try_into().unwrap());
        assert_eq!(id, u32::from(j > 29), "sample {sample}");
    }
    assert_eq!(npy_data(&members["ram.npy"]).len(), 60 * game.ram.len() * 8);
    // Every replayed frame's action; the frames holding start have none.
    let frame_direction = npy_data(&members["frame_direction.npy"]);
    let frame_buttons = npy_data(&members["frame_buttons.npy"]);
    let frame_labelled = npy_data(&members["frame_labelled.npy"]);
    assert_eq!(frame_labelled.len(), 248);
    for frame in 0..248 {
        let mask = masks[frame / 4];
        match space.action_of(mask) {
            Some(action) => {
                assert_eq!(frame_direction[frame], action.direction as u8);
                assert_eq!(frame_buttons[frame], action.buttons);
                assert_eq!(frame_labelled[frame], 1);
            }
            None => {
                assert_eq!(frame / 4, 30);
                assert_eq!((frame_labelled[frame], frame_buttons[frame]), (0, 0));
            }
        }
    }

    // Frames: the direct replay's, rendered by the same pipeline.
    let mut pipeline = gth_frame::Pipeline::new(
        60,
        80,
        options(gth_record::Steps::Recorded).crop,
        gth_frame::Rotation::Cw90,
        gth_frame::Filter::Area,
    )
    .unwrap();
    let mut rendered = Vec::new();
    let mut replayer = replay(&path, &content(&dir.0)).unwrap();
    while replayer.step().unwrap().is_some() {
        let mut frame = vec![0; pipeline.out_len()];
        replayer
            .core()
            .with_frame(|f| pipeline.render(f, &mut frame))
            .unwrap();
        rendered.push(frame);
    }
    drop(replayer);
    let expected: Vec<u8> = observed.iter().flat_map(|&j| rendered[j].clone()).collect();
    let decode = |members: &BTreeMap<String, Vec<u8>>, chunks: u32, samples: usize| {
        let mut decoded = Vec::new();
        for chunk in 0..chunks {
            let n = (samples - chunk as usize * 16).min(16);
            let mut out = vec![0; n * pipeline.out_len()];
            let data = &members[&gth_record::chunk_name(chunk)];
            gth_record::decode_frames(data, &mut out, &out_path(&dir)).unwrap();
            decoded.extend(out);
        }
        decoded
    };
    assert!(
        decode(&members, 4, 60) == expected,
        "decoded frames differ from the replay's"
    );

    // Every 8 frames over 4-frame steps: observation m is step 2m + 1's last frame, and its
    // label is the action of the frame right after it (step 2m + 2's), never a later one.
    let every8 = dir.0.join("every8.npz");
    let mut replayer = replay(&path, &content(&dir.0)).unwrap();
    let k = gth_record::Steps::Every(NonZeroU32::new(8).unwrap());
    let summary = gth_record::export(
        &mut replayer,
        &options(k),
        &mut gth_record::KeepAll,
        &every8,
    )
    .unwrap();
    drop(replayer);
    // 31 observations, the last without a following frame; m = 14 is followed by start.
    assert_eq!((summary.samples, summary.unlabelled), (29, 1));
    let members8 = shard_members(&every8);
    let observed8: Vec<usize> = (0..30).filter(|&m| m != 14).collect();
    let direction8 = npy_data(&members8["direction.npy"]);
    let buttons8 = npy_data(&members8["buttons.npy"]);
    for (sample, &m) in observed8.iter().enumerate() {
        let action = space.action_of(masks[2 * m + 2]).unwrap();
        assert_eq!(
            (direction8[sample], buttons8[sample]),
            (action.direction as u8, action.buttons)
        );
    }
    let expected8: Vec<u8> = observed8
        .iter()
        .flat_map(|&m| rendered[2 * m + 1].clone())
        .collect();
    assert!(decode(&members8, 2, 29) == expected8);
    let meta8: gth_record::ShardMeta = serde_json::from_slice(&members8["meta.json"]).unwrap();
    assert_eq!(meta8.frameskip, Some(8));

    // Every 4 frames is the recorded rate here. The playing filter keeps the observation
    // before the first input through game over: the first terminate after one that didn't.
    let every = dir.0.join("every.npz");
    let mut replayer = replay(&path, &content(&dir.0)).unwrap();
    let k = gth_record::Steps::Every(NonZeroU32::new(4).unwrap());
    let summary = gth_record::export(
        &mut replayer,
        &options(k),
        &mut gth_record::Playing::default(),
        &every,
    )
    .unwrap();
    drop(replayer);
    let ram = npy_data(&members["ram.npy"]);
    let names: Vec<String> = game.ram.keys().cloned().collect();
    let terminated = |sample: usize| {
        let values = (names.iter().enumerate())
            .map(|(i, name)| {
                let at = (sample * names.len() + i) * 8;
                (
                    name.clone(),
                    i64::from_le_bytes(ram[at..at + 8].try_into().unwrap()),
                )
            })
            .collect();
        game.terminated(&values).unwrap()
    };
    let first = observed.iter().position(|&j| masks[j + 1] != 0).unwrap();
    let armed = (first..60).find(|&s| !terminated(s)).unwrap();
    let game_over = (armed..60).find(|&s| terminated(s)).unwrap();
    assert!(
        observed[game_over] < 29,
        "the test core's game ends before the unlabelled sample"
    );
    assert_eq!(summary.samples, (game_over + 1 - first) as u64);
    assert_eq!(summary.filtered, 60 - summary.samples);
    let members_every = shard_members(&every);
    assert_eq!(
        npy_data(&members_every["direction.npy"]),
        &direction[first..=game_over]
    );
    let meta: gth_record::ShardMeta = serde_json::from_slice(&members_every["meta.json"]).unwrap();
    assert!(meta.terminated && meta.filter == "playing");
    // The all shard goes on past game over, so its last sample isn't terminated.
    assert_eq!(meta_terminated(&out), terminated(59));
}

fn meta_terminated(path: &Path) -> bool {
    let meta: gth_record::ShardMeta =
        serde_json::from_slice(&shard_members(path)["meta.json"]).unwrap();
    meta.terminated
}

fn out_path(dir: &TempDir) -> PathBuf {
    dir.0.join("export.npz")
}

#[test]
fn an_export_that_fails_leaves_no_shard() {
    let _serial = serial();
    let dir = TempDir::new();
    let game = game();
    let mut core = load(&game, &content(&dir.0));
    let path = dir.0.join("idle.gthrec");
    let mut recorder = Recorder::create(&path, &mut core, &game, None, info(60)).unwrap();
    recorder.run(&mut core, 10, 0).unwrap();
    recorder.finish(&mut core).unwrap();
    drop(core);
    let out = dir.0.join("bad.npz");
    let mut replayer = Replayer::new(
        Recording::open(&path).unwrap(),
        load(&game, &content(&dir.0)),
    )
    .unwrap();
    let options = gth_record::ShardOptions {
        obs_height: 60,
        obs_width: 80,
        // Wider than the test core's 320-pixel frame.
        crop: Some(gth_frame::Rect {
            x: 0,
            y: 0,
            width: 400,
            height: 10,
        }),
        rotation: gth_frame::Rotation::None,
        interpolation: gth_frame::Filter::Area,
        steps: gth_record::Steps::Every(NonZeroU32::new(1).unwrap()),
        chunk_samples: NonZeroU32::new(8).unwrap(),
        harness: BTreeMap::new(),
    };
    let error =
        gth_record::export(&mut replayer, &options, &mut gth_record::KeepAll, &out).unwrap_err();
    assert!(matches!(error, Error::Mismatch { .. }), "{error}");
    assert!(error.to_string().contains("crop"), "{error}");
    assert!(!out.exists() && !dir.0.join("bad.npz.partial").exists());
    // Exporting needs a replay from the start.
    let error =
        gth_record::export(&mut replayer, &options, &mut gth_record::KeepAll, &out).unwrap_err();
    assert!(error.to_string().contains("already started"), "{error}");
}

#[test]
fn a_rewound_recording_replays_as_one_run() {
    use gth_record::{RewindCause, RewindMeta};
    let _serial = serial();
    let dir = TempDir::new();
    let game = game();
    let content = content(&dir.0);
    let mut core = load(&game, &content);
    core.run_inputs(&game.reset_inputs().unwrap());
    let path = dir.0.join("rec.gthrec");
    let mut recorder = Recorder::create(&path, &mut core, &game, Some(&content), info(7)).unwrap();
    let mut rng = Rng(3);
    let mut random_steps = |recorder: &mut Recorder, core: &mut Core, steps: usize| {
        for _ in 0..steps {
            let frames = (1 + rng.next() % 6) as u32;
            recorder.run(core, frames, rng.next() as u16).unwrap();
        }
    };
    random_steps(&mut recorder, &mut core, 10);
    let branch = recorder.frames();
    let state = core.serialize().unwrap();
    random_steps(&mut recorder, &mut core, 10);
    let rewind = |frame, clip: &str| RewindMeta {
        frame,
        cause: RewindCause::Manual,
        clip: clip.into(),
    };
    // Only the end of a step, and not past the end.
    let end = recorder.frames();
    let mid_step = (branch + 1..end)
        .find(|&frame| !recorder.inputs()[frame as usize - 1].step_end)
        .unwrap();
    assert!(matches!(
        recorder.rewind(rewind(mid_step, "x")),
        Err(Error::Mismatch { .. })
    ));
    assert!(matches!(
        recorder.rewind(rewind(end + 1, "x")),
        Err(Error::Mismatch { .. })
    ));
    // A rewind to a later frame, then one to an earlier frame: the first leaves the timeline.
    random_steps(&mut recorder, &mut core, 3);
    let later = recorder.frames();
    let later_state = core.serialize().unwrap();
    random_steps(&mut recorder, &mut core, 5);
    core.unserialize(&later_state).unwrap();
    recorder.rewind(rewind(later, "later.gthrec")).unwrap();
    random_steps(&mut recorder, &mut core, 5);
    core.unserialize(&state).unwrap();
    recorder.rewind(rewind(branch, "branch.gthrec")).unwrap();
    assert_eq!(recorder.frames(), branch);
    assert_eq!(recorder.inputs().len() as u64, branch);
    assert_eq!(recorder.rewinds(), [rewind(branch, "branch.gthrec")]);
    random_steps(&mut recorder, &mut core, 10);
    let frames = recorder.frames();
    let path = recorder.finish(&mut core).unwrap();
    drop(core);

    // The checks (RAM hash and frame hash) after the rewinds came from the rewound states:
    // replaying the spliced log from the start reproduces them, the last one included.
    let recording = Recording::open(&path).unwrap();
    assert_eq!(recording.meta.frames, frames);
    assert_eq!(recording.meta.rewind, [rewind(branch, "branch.gthrec")]);
    assert_eq!(recording.meta.branch, None);
    let mut replayer = replay(&path, &content).unwrap();
    assert_eq!(replayer.run_to_end().unwrap(), frames);
}
