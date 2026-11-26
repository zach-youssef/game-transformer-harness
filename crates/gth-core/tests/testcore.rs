//! The host against the test core (docs/libretro.md, "Test core"), loaded from its built
//! library as a real core would be.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use gth_core::{Core, Error};
use gth_libretro_sys::*;

const WIDTH: usize = 320;
const HEIGHT: usize = 240;

/// One core per process: the tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn load(content: Option<&Path>) -> Core {
    load_with(content, &[]).expect("load the test core")
}

fn load_with(content: Option<&Path>, options: &[(&str, &str)]) -> gth_core::Result<Core> {
    let options = (options.iter())
        .map(|&(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    Core::load(
        gth_test_support::testcore_path(),
        content,
        &std::env::temp_dir(),
        &options,
    )
}

const fn bit(id: u32) -> u16 {
    1 << id
}
const RIGHT: u16 = bit(RETRO_DEVICE_ID_JOYPAD_RIGHT);
const DOWN: u16 = bit(RETRO_DEVICE_ID_JOYPAD_DOWN);
const A: u16 = bit(RETRO_DEVICE_ID_JOYPAD_A);
const START: u16 = bit(RETRO_DEVICE_ID_JOYPAD_START);

/// The last frame as XRGB8888 pixels.
fn pixels(core: &Core) -> Vec<u32> {
    core.with_frame(|frame| {
        assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));
        assert_eq!(frame.format, gth_frame::PixelFormat::Xrgb8888);
        let (pixels, _) = frame.data.as_chunks::<4>();
        pixels.iter().map(|&p| u32::from_ne_bytes(p)).collect()
    })
}

fn square(pixels: &[u32]) -> (usize, usize) {
    let index = pixels
        .iter()
        .position(|&p| p == 0x00FF_FFFF)
        .expect("square drawn");
    (index % WIDTH, index / WIDTH)
}

fn frame_hash(core: &Core) -> u64 {
    let mut hasher = DefaultHasher::new();
    core.with_frame(|frame| frame.data.hash(&mut hasher));
    hasher.finish()
}

fn ram(core: &mut Core) -> Vec<u8> {
    core.memory(RETRO_MEMORY_SYSTEM_RAM)
        .expect("system RAM")
        .to_vec()
}

#[test]
fn reports_system_and_av_info() {
    let _serial = serial();
    let core = load(None);
    let info = core.system_info();
    assert_eq!(info.library_name, "gth-testcore");
    assert_eq!(info.library_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(info.valid_extensions, "gthc");
    let av = core.av_info();
    assert_eq!((av.base_width, av.base_height), (320, 240));
    assert_eq!((av.fps, av.sample_rate), (60.0, 48_000.0));
    assert_eq!(core.path(), gth_test_support::testcore_path());
}

#[test]
fn frames_show_the_held_buttons_and_count() {
    let _serial = serial();
    let mut core = load(None);
    assert!(
        pixels(&core).iter().all(|&p| p == 0),
        "black before the first frame"
    );
    core.run_frames(1, A | START);
    let frame = pixels(&core);
    let cell = |id: u32| frame[id as usize * 20 + 10];
    assert_eq!(cell(RETRO_DEVICE_ID_JOYPAD_A), 0x00FF_0000 | (8 * 0x1010));
    assert_eq!(
        cell(RETRO_DEVICE_ID_JOYPAD_START),
        0x00FF_0000 | (3 * 0x1010)
    );
    assert_eq!(cell(RETRO_DEVICE_ID_JOYPAD_B), 0x0100, "unheld: background");
    assert_eq!(frame[WIDTH * 100], 0x0100, "green is frame number 1");
    assert_eq!(ram(&mut core)[..4], [0, 0, 0, 1], "A scored once");
    assert_eq!(core.audio_frames(), 800);
}

#[test]
fn run_frames_advances_n_frames_and_keeps_the_last() {
    let _serial = serial();
    let mut core = load(None);
    core.run_frames(4, RIGHT | DOWN);
    let frame = pixels(&core);
    assert_eq!(frame[WIDTH * 100], 0x0400, "frame 4");
    assert_eq!(
        square(&frame),
        (152 + 8, 112 + 8),
        "moved 2 pixels per frame"
    );
    core.run_frames(3, A);
    assert_eq!(pixels(&core)[WIDTH * 200], 0x0700);
    assert_eq!(ram(&mut core)[..4], [0, 0, 0, 3]);
    assert_eq!(core.frames_run(), 7);
    assert_eq!(core.audio_frames(), 7 * 800);
    core.run_frames(0, A);
    assert_eq!(core.frames_run(), 7, "0 frames runs nothing");
}

#[test]
fn savestate_round_trip_reproduces_frames_and_ram() {
    let _serial = serial();
    let content = std::env::temp_dir().join(format!("gth-core-test-{}.gthc", std::process::id()));
    std::fs::write(&content, b"seed").unwrap();
    let mut core = load(Some(&content));
    std::fs::remove_file(&content).unwrap();
    let inputs = [A, RIGHT, A | DOWN, 0, START, A];
    for &buttons in &inputs {
        core.run_frames(2, buttons);
    }
    let state = core.serialize().unwrap();
    let play = |core: &mut Core| {
        inputs.map(|buttons| {
            core.run_frames(3, buttons);
            (frame_hash(core), ram(core))
        })
    };
    let first = play(&mut core);
    assert_ne!(first[0].1[4..8], [0; 4], "content seeds the PRNG");
    core.unserialize(&state).unwrap();
    assert!(
        pixels(&core).iter().all(|&p| p == 0),
        "a state holds no frame"
    );
    assert!(first == play(&mut core));
    let error = core.unserialize(&state[..10]).unwrap_err();
    assert!(matches!(error, Error::Savestate { .. }), "{error}");
}

#[test]
fn deterministic_over_1000_frames_from_a_state() {
    let _serial = serial();
    let mut core = load(None);
    core.run_frames(5, RIGHT);
    let state = core.serialize().unwrap();
    let run = |core: &mut Core| {
        let mut hasher = DefaultHasher::new();
        for i in 0..1000u32 {
            // A varied but fixed input sequence.
            let buttons = (i.wrapping_mul(2_654_435_761) >> 16) as u16;
            core.run_frames(1, buttons);
            frame_hash(core).hash(&mut hasher);
            ram(core).hash(&mut hasher);
        }
        hasher.finish()
    };
    let first = run(&mut core);
    core.unserialize(&state).unwrap();
    assert_eq!(first, run(&mut core));
}

#[test]
fn reset_returns_to_the_loaded_state() {
    let _serial = serial();
    let mut core = load(None);
    core.run_frames(1, A | RIGHT);
    let first = (frame_hash(&core), ram(&mut core));
    core.run_frames(10, A);
    core.reset();
    core.run_frames(1, A | RIGHT);
    assert!(first == (frame_hash(&core), ram(&mut core)));
}

#[test]
fn reset_restores_the_load_snapshot() {
    let _serial = serial();
    let mut core = load(None);
    assert!(
        core.resets_to_load_state(),
        "the test core serializes from the start"
    );
    core.run_frames(3, A);
    core.reset();
    assert_eq!(ram(&mut core)[..4], [0; 4]);
    assert!(
        pixels(&core).iter().all(|&p| p == 0),
        "reset clears the frame"
    );
}

#[test]
fn each_load_gets_a_fresh_save_dir_removed_on_drop() {
    let _serial = serial();
    let core = load(None);
    let first = core.save_dir().to_owned();
    assert!(first.is_dir());
    assert_ne!(first, std::env::temp_dir());
    std::fs::write(first.join("nvram"), b"saved").unwrap();
    drop(core);
    assert!(!first.exists(), "removed with its files");
    let core = load(None);
    assert_ne!(core.save_dir(), first);
    assert!(
        core.save_dir().read_dir().unwrap().next().is_none(),
        "empty"
    );
}

#[test]
fn memory_map_reads_the_system_ram() {
    let _serial = serial();
    let mut core = load(None);
    core.run_frames(12, A);
    let regions = core.memory_regions();
    assert_eq!(regions.len(), 1);
    assert_eq!((regions[0].start, regions[0].len), (0, 256));
    assert_eq!(regions[0].flags, RETRO_MEMDESC_SYSTEM_RAM);
    assert_eq!(core.read_mapped(0, 4), Some(&[0, 0, 0, 0x12][..]));
    assert_eq!(core.read_mapped(252, 4).map(<[u8]>::len), Some(4));
    assert_eq!(core.read_mapped(253, 4), None, "past the end");
    assert_eq!(core.read_mapped(usize::MAX, 2), None);
    assert!(core.memory(RETRO_MEMORY_SAVE_RAM).is_none());
}

#[test]
fn one_core_per_process() {
    let _serial = serial();
    let core = load(None);
    let error = Core::load(
        gth_test_support::testcore_path(),
        None,
        Path::new("."),
        &BTreeMap::new(),
    )
    .err()
    .expect("second load fails");
    assert!(matches!(error, Error::CoreLoad { .. }));
    assert!(
        error.to_string().contains("another core is loaded"),
        "{error}"
    );
    drop(core);
    let mut core = load(None);
    core.run_frames(1, 0);
    assert_eq!(
        pixels(&core)[WIDTH * 100],
        0x0100,
        "a fresh core after the first unloads"
    );
}

#[test]
fn load_failures_release_the_claim() {
    let _serial = serial();
    let missing = Core::load(
        Path::new("no/such/core.so"),
        None,
        Path::new("."),
        &BTreeMap::new(),
    );
    assert!(matches!(missing, Err(Error::CoreLoad { .. })));
    let content = Path::new("no/such/content.gthc");
    let error = Core::load(
        gth_test_support::testcore_path(),
        Some(content),
        Path::new("."),
        &BTreeMap::new(),
    )
    .err()
    .expect("missing content fails");
    assert!(matches!(error, Error::Content { .. }), "{error}");
    assert!(
        error.to_string().contains("need_fullpath is false"),
        "{error}"
    );
    drop(load(None));
}

#[test]
fn core_options_are_set_before_the_core_reads_them() {
    let _serial = serial();
    let mut core = load_with(None, &[("gth_testcore_step", "4")]).unwrap();
    core.run_frames(1, RIGHT);
    assert_eq!(square(&pixels(&core)), (152 + 4, 112), "4 pixels per frame");
    drop(core);
    for (options, reason) in [
        (
            &[("gth_testcore_step", "3")],
            "isn't one of the core's values",
        ),
        (&[("no_such_option", "1")], "has no option no_such_option"),
    ] {
        let error = load_with(None, options).err().expect("bad option fails");
        assert!(matches!(error, Error::CoreLoad { .. }));
        assert!(error.to_string().contains(reason), "{error}");
    }
    drop(load(None)); // the failed loads released the claim
}

#[test]
fn run_inputs_plays_a_script() {
    let _serial = serial();
    let mut core = load(None);
    core.run_inputs(&[(A, 3), (0, 2), (A | RIGHT, 1)]);
    assert_eq!(core.frames_run(), 6);
    assert_eq!(ram(&mut core)[..4], [0, 0, 0, 4]);
    assert_eq!(square(&pixels(&core)), (154, 112));
}

#[test]
fn reads_ram_variables_from_memory_and_the_map() {
    let _serial = serial();
    let mut core = load(None);
    core.run_frames(123, A);
    let config = gth_config::GameConfig::parse(
        r#"
core = "gth_testcore"
orientation = 0
content = { file = "x.gthc", size = 0, sha1 = "0000000000000000000000000000000000000000", crc32 = "00000000" }
buttons = { a = "shot" }
reset = { script = [{ press = [], frames = 1 }] }
episode = { terminate = "score > 0" }
[ram]
score = { memory = "system_ram", offset = 0, size = 4, encoding = "bcd", scale = 10 }
mapped = { address = 2, size = 2, encoding = "be" }
swapped = { address = 2, size = 2, encoding = "be", swap16 = true }
past_end = { memory = "system_ram", offset = 254, size = 4, encoding = "le" }
unmapped = { address = 0x1000, size = 1, encoding = "le" }
save = { memory = "save_ram", offset = 0, size = 1, encoding = "le" }
"#,
        "test config",
    )
    .unwrap();
    let ram = &config.ram;
    assert_eq!(core.read_ram("score", &ram["score"]).unwrap(), 1230);
    assert_eq!(core.read_ram("mapped", &ram["mapped"]).unwrap(), 0x0123);
    assert_eq!(core.read_ram("swapped", &ram["swapped"]).unwrap(), 0x2301);
    for (name, reason) in [
        ("past_end", "past the end of SystemRam (256 bytes)"),
        ("unmapped", "aren't in one memory map region"),
        ("save", "exposes no SaveRam"),
    ] {
        let error = core.read_ram(name, &ram[name]).unwrap_err();
        assert!(matches!(error, Error::Ram { .. }));
        assert!(
            error
                .to_string()
                .starts_with(&format!("cannot read RAM variable {name}: ")),
            "{error}"
        );
        assert!(error.to_string().contains(reason), "{error}");
    }
    let mut vars = ram.clone();
    vars.retain(|name, _| ["score", "mapped"].contains(&name.as_str()));
    let values = core.read_ram_vars(&vars).unwrap();
    assert_eq!(
        values,
        BTreeMap::from([("mapped".into(), 0x0123), ("score".into(), 1230)])
    );
    assert!(core.read_ram_vars(ram).is_err());
    core.check_ram_vars(&vars).unwrap();
    let error = core.check_ram_vars(ram).unwrap_err();
    assert!(matches!(error, Error::Ram { .. }), "{error}");
}

/// The candidates as `(location, view, value)`, for exact comparisons.
fn candidates(search: &gth_core::RamSearch) -> Vec<(gth_config::Location, String, i64)> {
    (search.candidates())
        .map(|c| (c.location, c.view.to_string(), c.value))
        .collect()
}

fn script(text: &str) -> gth_config::SearchScript {
    gth_config::SearchScript::parse(text, "test script").unwrap()
}

#[test]
fn ram_search_finds_the_bcd_score_from_a_scripted_session() {
    use gth_config::{Location, MemoryId};
    let _serial = serial();
    let dir = tempdir("ramsearch-score");
    let content = dir.join("seed.gthc");
    std::fs::write(&content, b"seed").unwrap();
    // With content the PRNG changes RAM bytes 4..8 every frame, which `equal` drops.
    let mut core = load(Some(&content));
    let steps = r#"
[[step]]
press = ["a"]
frames = 150
expect = "increased"
[[step]]
frames = 60
expect = "equal"
[[step]]
press = ["a"]
frames = 7
expect = "increased"
[[step]]
expect = "equals"
value = 157
"#;
    let system_ram = |offset| Location::Memory {
        memory: MemoryId::SystemRam,
        offset,
    };
    let spaces: [(&str, &dyn Fn(usize) -> Location); 2] = [
        ("system_ram", &system_ram),
        ("memory_map", &Location::Address),
    ];
    for (space, at) in spaces {
        core.reset();
        let text = format!("space = \"{space}\"\nviews = [\"all\"]\n{steps}");
        let (search, counts) = gth_core::RamSearch::run_script(&mut core, &script(&text), &dir)
            .expect("run the script");
        assert!(
            counts.windows(2).all(|pair| pair[0] >= pair[1]),
            "{counts:?}"
        );
        // The score, 00 00 01 57: all four bytes as BCD, and the low two (equal while the
        // score is below 10000).
        assert_eq!(
            candidates(&search),
            [(at(2), "bcd2".into(), 157), (at(0), "bcd4".into(), 157)],
            "{space}"
        );
        let bcd4 = search.candidates().last().unwrap();
        assert_eq!(core.read_ram("score", &bcd4.ram_var()).unwrap(), 157);
        // Watching the score (bcd2 first) while A is held shows it count up, frame by frame.
        let values = search.watch(&mut core, &[(A, 3)], 1).unwrap();
        assert_eq!(values, [[Some(158)], [Some(159)], [Some(160)]]);
    }
    drop(core);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ram_search_steps_without_expect_drop_bcd_that_stops_decoding() {
    let _serial = serial();
    let dir = tempdir("ramsearch-rebase");
    let content = dir.join("seed.gthc");
    std::fs::write(&content, b"seed").unwrap();
    let mut core = load(Some(&content));
    // The PRNG bytes change every frame, so some stop being BCD by the last step.
    let text = r#"
space = "system_ram"
views = ["bcd1"]
[[step]]
frames = 30
[[step]]
frames = 30
"#;
    let (search, counts) = gth_core::RamSearch::run_script(&mut core, &script(text), &dir).unwrap();
    assert!(counts[2] < counts[0], "{counts:?}");
    assert_eq!(
        candidates(&search).len(),
        counts[2],
        "every candidate decodes"
    );
    drop(core);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ram_search_narrows_on_savestate_pairs() {
    use gth_config::{Location, MemoryId};
    let _serial = serial();
    let dir = tempdir("ramsearch-states");
    let mut core = load(None);
    core.run_frames(3, A);
    std::fs::write(dir.join("after.state"), core.serialize().unwrap()).unwrap();
    core.run_frames(2, A);
    std::fs::write(dir.join("before.state"), core.serialize().unwrap()).unwrap();
    core.run_frames(10, A);
    // Like a death: the value drops from `before` (5) to `after` (3).
    let text = r#"
space = "system_ram"
views = ["bcd4"]
[[step]]
state = "before.state"
[[step]]
state = "after.state"
expect = "decreased"
[[step]]
expect = "equals"
value = 3
"#;
    let (search, counts) = gth_core::RamSearch::run_script(&mut core, &script(text), &dir).unwrap();
    // Every window holding byte 3 decreased (5 -> 3, 500 -> 300, ...); only one reads 3.
    assert_eq!(counts, [253, 253, 4, 1]);
    let offset0 = Location::Memory {
        memory: MemoryId::SystemRam,
        offset: 0,
    };
    assert_eq!(candidates(&search), [(offset0, "bcd4".into(), 3)]);

    let missing = text.replace("after.state", "missing.state");
    let error = gth_core::RamSearch::run_script(&mut core, &script(&missing), &dir)
        .err()
        .expect("a missing savestate fails");
    assert!(matches!(error, Error::RamSearch { .. }), "{error}");
    assert!(error.to_string().contains("missing.state"), "{error}");
    drop(core);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ram_search_needs_the_memory() {
    let _serial = serial();
    let mut core = load(None);
    let views = ["bcd4".parse().unwrap()];
    let space = gth_config::Space::Memory(gth_config::MemoryId::SaveRam);
    let error = gth_core::RamSearch::new(&mut core, space, &views)
        .err()
        .expect("the test core has no save RAM");
    assert_eq!(
        error.to_string(),
        "RAM search: the core exposes no save_ram"
    );
    let error = gth_core::RamSearch::new(&mut core, space, &[])
        .err()
        .expect("no views");
    assert!(error.to_string().contains("views"), "{error}");

    // A watch longer than the cap fails before running a frame.
    let space = gth_config::Space::Memory(gth_config::MemoryId::SystemRam);
    let search = gth_core::RamSearch::new(&mut core, space, &views).unwrap();
    let half = gth_core::RamSearch::MAX_WATCH_FRAMES / 2 + 1;
    let error = (search.watch(&mut core, &[(A, half), (0, half)], 1)).expect_err("too many frames");
    assert!(error.to_string().contains("watch at most"), "{error}");
    // So does one whose values (candidates times frames) pass the cap.
    let frames = (gth_core::RamSearch::MAX_WATCH_VALUES / search.len() as u64 + 1) as u32;
    assert!(frames <= gth_core::RamSearch::MAX_WATCH_FRAMES);
    let error =
        (search.watch(&mut core, &[(A, frames)], search.len())).expect_err("too many values");
    assert!(error.to_string().contains("values"), "{error}");
    assert_eq!(core.frames_run(), 0);
}

/// A new, empty directory under the system's temporary directory, unique to this process.
fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("gth-core-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
