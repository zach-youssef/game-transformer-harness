//! Sprite streams against the test core's marker list (docs/libretro.md, "Test core"), which
//! is buffered after each frame and drawn in the next, as Psikyo's lists are.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use gth_config::SpriteHw;
use gth_core::Core;
use gth_libretro_sys::*;
use gth_sprites::{Error, Sprite, SpriteStream, VISIBLE};

/// One core per process: the tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn load(sprites: &str) -> Core {
    let options = BTreeMap::from([("gth_testcore_sprites".to_owned(), sprites.to_owned())]);
    Core::load(
        gth_test_support::testcore_path(),
        None,
        &std::env::temp_dir(),
        &options,
        0,
    )
    .expect("load the test core")
}

const RIGHT: u16 = 1 << RETRO_DEVICE_ID_JOYPAD_RIGHT;
const SELECT: u16 = 1 << RETRO_DEVICE_ID_JOYPAD_SELECT;
const MAGENTA: u32 = 0x00FF_00FF;

fn pixel(core: &Core, x: usize, y: usize) -> u32 {
    core.with_frame(|frame| {
        let at = y * frame.pitch + 4 * x;
        u32::from_ne_bytes(frame.data[at..at + 4].try_into().unwrap())
    })
}

fn boxes(sprites: &[Sprite]) -> Vec<(i16, i16, u16, u16)> {
    sprites
        .iter()
        .map(|s| (s.x, s.y, s.width, s.height))
        .collect()
}

/// Reading before a frame gives the markers that frame draws, wherever the square has moved
/// in that frame.
#[test]
fn a_read_before_a_frame_gives_the_sprites_it_draws() {
    let _serial = serial();
    let mut core = load("on");
    let stream = SpriteStream::bind(SpriteHw::Testcore, &mut core).unwrap();
    assert_eq!(stream.screen(), [320, 240]);
    let mut sprites = Vec::new();
    stream.read(&mut core, &mut sprites).unwrap();
    // The square starts at (152, 112): its marker is the 4x4 at its centre.
    assert_eq!(boxes(&sprites), [(158, 118, 4, 4)]);
    assert_eq!(sprites[0].flags, VISIBLE);

    for frame in 0..4 {
        let buttons = if frame % 2 == 0 {
            RIGHT | SELECT
        } else {
            RIGHT
        };
        stream.read(&mut core, &mut sprites).unwrap();
        core.run_frames(1, buttons);
        for sprite in &sprites {
            let (x, y) = (sprite.x as usize, sprite.y as usize);
            for (dx, dy) in [(0, 0), (3, 3)] {
                assert_eq!(
                    pixel(&core, x + dx, y + dy),
                    MAGENTA,
                    "frame {frame}: {sprite:?}"
                );
            }
        }
    }
    // Two frames added a life each, and the square moved 8 pixels right.
    stream.read(&mut core, &mut sprites).unwrap();
    assert_eq!(
        boxes(&sprites),
        [(166, 118, 4, 4), (4, 10, 4, 4), (10, 10, 4, 4)]
    );
    assert_eq!(sprites[2].code, 2);
}

/// The list derives from what savestates hold, so after a load it is the saved run's.
#[test]
fn a_loaded_state_gives_its_own_list() {
    let _serial = serial();
    let mut core = load("on");
    let stream = SpriteStream::bind(SpriteHw::Testcore, &mut core).unwrap();
    core.run_frames(3, RIGHT | SELECT);
    let state = core.serialize().unwrap();
    let mut saved = Vec::new();
    stream.read(&mut core, &mut saved).unwrap();
    core.run_frames(5, 0);
    core.reset();
    let mut sprites = Vec::new();
    stream.read(&mut core, &mut sprites).unwrap();
    assert_ne!(sprites, saved);
    core.unserialize(&state).unwrap();
    stream.read(&mut core, &mut sprites).unwrap();
    assert_eq!(sprites, saved);
}

/// A core without the area interface, or without the board's areas, has no stream, and the
/// error says why.
#[test]
fn binding_fails_with_the_reason() {
    let _serial = serial();
    let mut core = load("off");
    assert!(!core.has_areas());
    let error = SpriteStream::bind(SpriteHw::Testcore, &mut core)
        .err()
        .unwrap();
    assert!(matches!(error, Error::Unavailable { .. }));
    assert!(
        error.to_string().contains("no named-area interface"),
        "{error}"
    );
    drop(core);

    let mut core = load("on");
    assert!(core.area("no such area").is_none());
    let error = SpriteStream::bind(SpriteHw::Psikyo, &mut core)
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("no psikyo sprite stream from core gth-testcore"),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("no area \"Psikyo sprite lists\""),
        "{error}"
    );
}
