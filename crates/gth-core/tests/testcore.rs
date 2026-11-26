//! The host against the test core (docs/libretro.md, "Test core"), loaded from its built
//! library as a real core would be.

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
    Core::load(
        gth_test_support::testcore_path(),
        content,
        &std::env::temp_dir(),
    )
    .expect("load the test core")
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
    let error = Core::load(gth_test_support::testcore_path(), None, Path::new("."))
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
    let missing = Core::load(Path::new("no/such/core.so"), None, Path::new("."));
    assert!(matches!(missing, Err(Error::CoreLoad { .. })));
    let content = Path::new("no/such/content.gthc");
    let error = Core::load(
        gth_test_support::testcore_path(),
        Some(content),
        Path::new("."),
    )
    .err()
    .expect("missing content fails");
    assert!(error.to_string().contains("cannot read"), "{error}");
    drop(load(None));
}
