//! gth-play's rewind on the test core (docs/human-play.md, "Rewind"): a drop of the test
//! core's lives byte (docs/libretro.md, "Test core") or the hotkey rewinds the recording,
//! which still verifies as one run, and the rewound frames replay as a clip to the death.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use gth_config::{GameConfig, RetroButton};
use gth_core::Core;
use gth_play::game::GamePaths;
use gth_play::session::{Event, RING_REWINDS, RecordSettings, RewindMode, SNAPSHOT_EVERY, Session};
use gth_record::{BranchMeta, RecorderKind, Recording, Replayer, RewindCause, RewindMeta};

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
        let path = std::env::temp_dir().join(format!("gth-play-rewind-{}-{n}", std::process::id()));
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

/// Read at run time, as in tests/testcore.rs.
fn games_dir() -> PathBuf {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo test sets it");
    Path::new(&manifest).join("../../games")
}

/// The testcore game, with its lives byte as `lives` when `lives`.
fn game(lives: bool) -> GameConfig {
    let path = games_dir().join("testcore/game.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    if lives {
        text = text.replace(
            "[episode]",
            "[ram.lives]\nmemory = \"system_ram\"\noffset = 8\nsize = 1\nencoding = \"le\"\n\n\
             [episode]",
        );
    }
    let game = GameConfig::parse(&text, "testcore").unwrap();
    assert_eq!(game.ram.contains_key("lives"), lives);
    game
}

fn session(out: &Path, rewind_frames: u64, lives: bool) -> Session {
    let game = game(lives);
    let core = Core::load(
        gth_test_support::testcore_path(),
        None,
        &std::env::temp_dir(),
        &game.core_options,
        game.settle_frames,
    )
    .unwrap();
    let settings = RecordSettings {
        dir: out.to_owned(),
        recorder: RecorderKind::Human,
        player: "test".into(),
        license: "CC0-1.0".into(),
        checks_every: NonZeroU32::new(7).unwrap(),
        core_lock: None,
        rewind_frames,
    };
    Session::new(core, game, "testcore", None, settings)
}

const SELECT: u16 = 1 << 2;
const L: u16 = 1 << 10;

/// Inputs that change the state every frame: moves and score (A), varying with `seed`.
fn mask(frame: u64, seed: u64) -> u16 {
    let a = RetroButton::A.bit();
    let dirs = [
        RetroButton::Right.bit(),
        RetroButton::Down.bit(),
        RetroButton::Left.bit(),
        RetroButton::Up.bit(),
    ];
    let x = frame.wrapping_mul(0x9e37_79b9).wrapping_add(seed);
    dirs[(x >> 3) as usize % 4] | if x & 4 == 0 { a } else { 0 }
}

/// Runs `masks` one frame each and returns the events of automatic rewinds.
fn play(session: &mut Session, masks: impl IntoIterator<Item = u16>) -> Vec<Event> {
    let mut events = Vec::new();
    for buttons in masks {
        assert!(session.run_frame(buttons).unwrap(), "not paused");
        events.extend(session.take_events());
    }
    events
}

fn recording_dir(session: &Session) -> PathBuf {
    session.recording().unwrap().0.to_owned()
}

fn rewound(event: &Event) -> (u64, u64, RewindCause, PathBuf) {
    match event {
        Event::Rewound {
            from,
            to,
            cause,
            clip,
        } => (*from, *to, *cause, clip.clone()),
        other => panic!("not a rewind: {other:?}"),
    }
}

fn name(path: &Path) -> String {
    path.file_name().unwrap().to_str().unwrap().to_owned()
}

/// Replays a recording as `gth-play --verify` does: from its start state, failing on a
/// desync at any check. Returns its frames. No other core may be loaded.
fn verify(dir: &Path) -> u64 {
    let paths = GamePaths {
        games_dir: games_dir(),
        data_dir: std::env::temp_dir(),
        rom_dir: None,
        core: Some(gth_test_support::testcore_path().into()),
    };
    gth_play::game::verify(dir, &paths).unwrap()
}

/// `lives` before the first and after the last frame of a recording's replay.
fn lives_at_ends(dir: &Path) -> (i64, i64) {
    let recording = Recording::open(dir).unwrap();
    let core = Core::load(
        gth_test_support::testcore_path(),
        None,
        &std::env::temp_dir(),
        &recording.game.core_options,
        recording.game.settle_frames,
    )
    .unwrap();
    let mut replayer = Replayer::new(recording, core).unwrap();
    let first = replayer.read_ram_vars().unwrap()["lives"];
    replayer.run_to_end().unwrap();
    (first, replayer.read_ram_vars().unwrap()["lives"])
}

#[test]
fn a_lives_drop_rewinds_and_its_clip_replays_to_the_death() {
    let _serial = serial();
    let out = TempDir::new();
    let mut session = session(&out.0, 45, true);
    assert_eq!(session.rewind_mode(), RewindMode::Auto);
    assert!(matches!(
        session.toggle_recording(),
        Ok(Event::RecordingStarted(_))
    ));
    let main = recording_dir(&session);
    // A life, then play to frame 106: no rewind, the count only went up.
    let mut masks: Vec<u16> = vec![SELECT];
    masks.extend((1..106).map(|f| mask(f, 1)));
    assert_eq!(play(&mut session, masks.clone()), []);
    // The death on frame 107: back 45 frames, to 62.
    masks.push(L | mask(106, 1));
    let events = play(&mut session, [L | mask(106, 1)]);
    let [event] = &events[..] else {
        panic!("{events:?}")
    };
    let (from, to, cause, clip) = rewound(event);
    assert_eq!((from, to, cause), (107, 62, RewindCause::Lives));
    assert_eq!(
        session.recording().unwrap().1,
        62,
        "truncated to the branch"
    );
    assert!(session.is_paused(), "a rewind pauses");
    assert!(!session.run_frame(0).unwrap());
    session.toggle_pause();
    // Play on differently from frame 62 (no death this time).
    let after: Vec<u16> = (62..120).map(|f| mask(f, 2)).collect();
    assert_eq!(play(&mut session, after.clone()), []);
    assert_eq!(session.close().unwrap(), Some(main.clone()));
    drop(session);

    let recording = Recording::open(&main).unwrap();
    assert_eq!(
        recording.meta.rewind,
        [RewindMeta {
            frame: 62,
            cause: RewindCause::Lives,
            clip: name(&clip),
        }]
    );
    assert_eq!(recording.meta.branch, None);
    let spliced: Vec<u16> = masks[..62].iter().chain(&after).copied().collect();
    let logged: Vec<u16> = recording.inputs.iter().map(|i| i.buttons).collect();
    assert_eq!(logged, spliced);
    assert_eq!(verify(&main), 62 + 58);

    let clip_recording = Recording::open(&clip).unwrap();
    assert_eq!(
        clip_recording.meta.branch,
        Some(BranchMeta {
            parent: name(&main),
            frame: 62,
            cause: RewindCause::Lives,
        })
    );
    assert_eq!(clip_recording.meta.rewind, []);
    let clip_inputs: Vec<u16> = clip_recording.inputs.iter().map(|i| i.buttons).collect();
    assert_eq!(clip_inputs, masks[62..107], "the frames up to the death");
    assert!(name(&clip).starts_with(name(&main).trim_end_matches(".gthrec")));
    assert_eq!(verify(&clip), 45);
    assert_eq!(lives_at_ends(&clip), (1, 0), "the clip ends on the death");

    // Its start state is the main recording's, replayed to the branch frame.
    let core = Core::load(
        gth_test_support::testcore_path(),
        None,
        &std::env::temp_dir(),
        &recording.game.core_options,
        recording.game.settle_frames,
    )
    .unwrap();
    let mut replayer = Replayer::new(recording, core).unwrap();
    for _ in 0..62 {
        replayer.run_frame().unwrap();
    }
    let ram = replayer.read_ram_vars().unwrap();
    drop(replayer);
    let mut core = Core::load(
        gth_test_support::testcore_path(),
        None,
        &std::env::temp_dir(),
        &clip_recording.game.core_options,
        clip_recording.game.settle_frames,
    )
    .unwrap();
    core.unserialize(&clip_recording.start_state).unwrap();
    assert_eq!(core.read_ram_vars(&clip_recording.game.ram).unwrap(), ram);
}

#[test]
fn the_hotkey_rewinds_a_game_without_lives_and_the_cli_verifies_it() {
    let _serial = serial();
    let out = TempDir::new();
    let mut session = session(&out.0, 45, false);
    assert!(matches!(session.rewind_mode(), RewindMode::ManualOnly(why) if why.contains("lives")));
    assert!(session.rewind().unwrap_err().contains("not recording"));
    session.toggle_recording().unwrap();
    let main = recording_dir(&session);
    // Select then L would be a death with a lives variable; here it's nothing.
    let masks: Vec<u16> = [SELECT, L]
        .into_iter()
        .chain((2..100).map(|f| mask(f, 3)))
        .collect();
    assert_eq!(play(&mut session, masks.clone()), []);
    let (from, to, cause, clip) = rewound(&session.rewind().unwrap());
    assert_eq!((from, to, cause), (100, 55, RewindCause::Manual));
    assert!(session.is_paused());
    session.toggle_pause();
    play(&mut session, (55..90).map(|f| mask(f, 4)));
    session.close().unwrap();
    drop(session);

    let recording = Recording::open(&clip).unwrap();
    assert_eq!(recording.meta.branch.unwrap().cause, RewindCause::Manual);
    assert_eq!(verify(&clip), 45);
    let output = Command::new(env!("CARGO_BIN_EXE_gth-play"))
        .arg("--verify")
        .arg(&main)
        .arg("--data-dir")
        .arg(&out.0)
        .arg("--core")
        .arg(gth_test_support::testcore_path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("90 frames replayed, no desync"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_rewind_stops_at_the_recording_start() {
    let _serial = serial();
    let out = TempDir::new();
    let mut session = session(&out.0, 45, true);
    session.toggle_recording().unwrap();
    let main = recording_dir(&session);
    play(&mut session, (0..20).map(|f| mask(f, 5)));
    let (from, to, _, clip) = rewound(&session.rewind().unwrap());
    assert_eq!((from, to), (20, 0));
    assert_eq!(session.recording().unwrap().1, 0);
    let error = session.rewind().unwrap_err();
    assert!(error.contains("nothing to rewind"), "{error}");
    session.toggle_pause();
    play(&mut session, (0..10).map(|f| mask(f, 6)));
    session.close().unwrap();
    drop(session);
    assert_eq!(verify(&main), 10);
    assert_eq!(verify(&clip), 20);
    assert_eq!(
        Recording::open(&clip).unwrap().meta.branch.unwrap().frame,
        0
    );
}

#[test]
fn a_second_death_inside_the_window_rewinds_past_the_first_branch() {
    let _serial = serial();
    let out = TempDir::new();
    let mut session = session(&out.0, 45, true);
    session.toggle_recording().unwrap();
    let main = recording_dir(&session);
    let mut first: Vec<u16> = vec![SELECT, SELECT];
    first.extend((2..99).map(|f| mask(f, 7)));
    first.push(L);
    let events = play(&mut session, first);
    let (from, to, _, clip1) = rewound(&events[0]);
    assert_eq!((events.len(), from, to), (1, 100, 55));
    session.toggle_pause();
    // Ten frames on, the second life goes: back to 21, before the first branch.
    let mut second: Vec<u16> = (55..65).map(|f| mask(f, 8)).collect();
    second.push(L);
    let events = play(&mut session, second);
    let (from, to, _, clip2) = rewound(&events[0]);
    assert_eq!((events.len(), from, to), (1, 66, 21));
    session.toggle_pause();
    play(&mut session, (21..80).map(|f| mask(f, 9)));
    session.close().unwrap();
    drop(session);

    let recording = Recording::open(&main).unwrap();
    assert_eq!(
        recording.meta.rewind,
        [RewindMeta {
            frame: 21,
            cause: RewindCause::Lives,
            clip: name(&clip2),
        }],
        "the first rewind's frame left the timeline"
    );
    assert_eq!(verify(&main), 80);
    // Both clips stay; the first is labelled with a frame of the replaced timeline.
    assert_ne!(clip1, clip2);
    assert_eq!(
        Recording::open(&clip1).unwrap().meta.branch.unwrap().frame,
        55
    );
    assert_eq!(verify(&clip1), 45);
    assert_eq!(lives_at_ends(&clip1), (2, 1));
    assert_eq!(verify(&clip2), 45);
    // The first death was rewound away, so the second costs the second of two lives too.
    assert_eq!(lives_at_ends(&clip2), (2, 1));
}

/// A death one frame past the rewind window branches at frame 1, which only the start
/// savestate precedes by less than 1 + settle_frames: that savestate serves it, as the
/// recording began from the same settled load.
#[test]
fn a_death_just_past_the_window_rewinds_to_frame_one_from_the_start_state() {
    let _serial = serial();
    let out = TempDir::new();
    let rewind = 45;
    let mut session = session(&out.0, rewind, true);
    session.toggle_recording().unwrap();
    let main = recording_dir(&session);
    let mut masks: Vec<u16> = vec![SELECT, SELECT];
    masks.extend((2..rewind).map(|f| mask(f, 13)));
    masks.push(L);
    let events = play(&mut session, masks);
    let (from, to, cause, clip) = rewound(&events[0]);
    assert_eq!(
        (events.len(), from, to, cause),
        (1, rewind + 1, 1, RewindCause::Lives)
    );
    assert_eq!(session.recording().unwrap().1, 1);
    session.toggle_pause();
    play(&mut session, (1..60).map(|f| mask(f, 14)));
    session.close().unwrap();
    drop(session);
    assert_eq!(verify(&main), 60);
    assert_eq!(verify(&clip), rewind);
    assert_eq!(
        Recording::open(&clip).unwrap().meta.branch.unwrap().frame,
        1
    );
    // From frame 1, after the first Select: the second adds a life, L takes it.
    assert_eq!(lives_at_ends(&clip), (1, 1));
}

#[test]
fn rewinds_stop_at_the_oldest_savestate_in_the_ring() {
    let _serial = serial();
    let out = TempDir::new();
    let rewind = 40;
    let mut session = session(&out.0, rewind, false);
    session.toggle_recording().unwrap();
    let main = recording_dir(&session);
    play(&mut session, (0..300).map(|f| mask(f, 10)));
    // The ring keeps RING_REWINDS rewinds and one more savestate: 7, from frame 120.
    let depth = (RING_REWINDS * rewind).div_ceil(SNAPSHOT_EVERY) + 1;
    let oldest = 300 - (depth - 1) * SNAPSHOT_EVERY;
    assert_eq!(oldest, 120);
    let mut rewinds = Vec::new();
    while let Ok(event) = session.rewind() {
        let (from, to, _, _) = rewound(&event);
        rewinds.push((from, to));
    }
    assert_eq!(
        rewinds,
        [
            (300, 260),
            (260, 220),
            (220, 180),
            (180, 140),
            (140, oldest + 2)
        ],
        "1 + settle_frames past the oldest savestate, so the frame there is the recording's"
    );
    session.toggle_pause();
    play(&mut session, (0..30).map(|f| mask(f, 11)));
    session.close().unwrap();
    drop(session);
    let recording = Recording::open(&main).unwrap();
    assert_eq!(recording.meta.rewind.len(), 1);
    assert_eq!(recording.meta.rewind[0].frame, oldest + 2);
    assert_eq!(verify(&main), oldest + 2 + 30);
}

/// Finishing right after a rewind checks the frame shown at the branch, which the core
/// must have drawn, not a cleared picture from a loaded savestate: 122 isn't a multiple of
/// 7, so the final check holds the picture's hash.
#[test]
fn a_recording_finished_at_the_oldest_reachable_frame_verifies() {
    let _serial = serial();
    let out = TempDir::new();
    let mut session = session(&out.0, 40, false);
    session.toggle_recording().unwrap();
    let main = recording_dir(&session);
    play(&mut session, (0..300).map(|f| mask(f, 12)));
    while session.rewind().is_ok() {}
    // The oldest savestate is at 120, and a seek replays 1 + settle_frames (1) after it.
    assert_eq!(session.recording().unwrap().1, 122);
    session.close().unwrap();
    drop(session);
    assert_eq!(verify(&main), 122);
}

/// A clip that can't be made leaves play where it was: the death frame's picture redrawn,
/// so finishing there verifies, and the directory in the clip's way untouched.
#[test]
fn a_failed_automatic_rewind_warns_and_play_goes_on() {
    let _serial = serial();
    let out = TempDir::new();
    let mut session = session(&out.0, 45, true);
    session.toggle_recording().unwrap();
    let main = recording_dir(&session);
    let stem = name(&main);
    let taken = out.0.join(format!(
        "{}-rewind-001.gthrec",
        stem.trim_end_matches(".gthrec")
    ));
    std::fs::create_dir(&taken).unwrap();
    std::fs::write(taken.join("mine"), "not the player's").unwrap();
    let mut masks: Vec<u16> = vec![SELECT];
    masks.extend((1..99).map(|f| mask(f, 13)));
    masks.push(L);
    let events = play(&mut session, masks);
    let [Event::Warning(warning)] = &events[..] else {
        panic!("{events:?}")
    };
    assert!(warning.contains("play goes on"), "{warning}");
    assert!(!session.is_paused());
    assert_eq!(session.recording().unwrap().1, 100, "not rewound");
    session.close().unwrap();
    drop(session);
    assert!(taken.join("mine").exists());
    assert_eq!(verify(&main), 100);
}
