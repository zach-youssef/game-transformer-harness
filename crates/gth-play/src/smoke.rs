//! `gth-play --headless-smoke`: the player's whole frame loop on the test core, with
//! scripted controller input instead of a window, audio device and controllers, so CI can
//! run it on every OS (docs/human-play.md, "Headless smoke").

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use gth_config::RetroButton;
use gth_core::Core;
use gth_record::RecorderKind;

use crate::display::Screen;
use crate::input::{LEFT, RIGHT, StickMap, clean_opposites};
use crate::session::{Event, RecordSettings, Session};

/// Frames recorded.
pub const FRAMES: u32 = 600;
/// Frames run before the recording starts, so it starts mid-game from a savestate.
pub const LEAD_IN: u32 = 30;

/// The stick settings of the scripted controller.
pub fn stick_map() -> StickMap {
    StickMap::new(0.5, 45.0).expect("valid")
}

/// The mask the player gives the core on recorded frame `i`: a stick turning 7 degrees a
/// frame (inside the deadzone a third of the time), A held in bursts, B tapped, and every
/// 100th frame left and right together, which cancel. Built as the player builds masks.
pub fn smoke_mask(i: u32) -> u16 {
    let angle = (i as f32 * 7.0).to_radians();
    let distance = if (i / 50).is_multiple_of(3) { 0.3 } else { 0.9 };
    let mut mask = stick_map().dpad(distance * angle.cos(), distance * angle.sin());
    if (i / 10).is_multiple_of(2) {
        mask |= RetroButton::A.bit();
    }
    if i % 37 < 5 {
        mask |= RetroButton::B.bit();
    }
    if i % 100 == 99 {
        mask |= LEFT | RIGHT;
    }
    clean_opposites(mask)
}

/// Runs the smoke with the test core at `core_path` and the `testcore` game from
/// `games_dir`, writing the recording (and the state slot) into `out`. Returns the
/// recording's directory.
pub fn run(core_path: &Path, games_dir: &Path, out: &Path) -> Result<PathBuf, String> {
    let game = crate::game::load_config(games_dir, "testcore")?;
    let core = Core::load(core_path, None, &std::env::temp_dir(), &game.core_options)
        .map_err(|e| format!("cannot load {}: {e}", core_path.display()))?;
    let settings = RecordSettings {
        dir: out.to_owned(),
        // Honest labelling: no human played this.
        recorder: RecorderKind::Scripted,
        player: "gth-play headless smoke".into(),
        license: "CC0-1.0".into(),
        checks_every: NonZeroU32::new(60).expect("non-zero"),
    };
    let mut screen = Screen::new(&game);
    let mut session = Session::new(core, game, "testcore", None, settings);
    session.core_mut().set_audio_capture(true);
    let (mut audio, mut window) = (Vec::new(), vec![0u32; 800 * 600]);
    let mut frame = |session: &mut Session, mask: u16| -> Result<(), String> {
        if session.run_frame(mask)? {
            audio.clear();
            session.core_mut().take_audio(&mut audio);
            if audio.is_empty() {
                return Err("the core gave no audio".into());
            }
        }
        session.core().with_frame(|f| screen.convert(f))?;
        screen.draw(&mut window, 800, 600);
        Ok(())
    };
    for _ in 0..LEAD_IN {
        frame(&mut session, 0)?;
    }
    session.save_state()?;
    let Event::RecordingStarted(dir) = session.toggle_recording()? else {
        unreachable!("no recording was running");
    };
    for i in 0..FRAMES {
        if i == FRAMES / 2 {
            // The hotkeys a player uses mid-recording: neither may add a frame.
            session.save_state()?;
            session.toggle_pause();
            frame(&mut session, RetroButton::Start.bit())?;
            session.toggle_pause();
        }
        frame(&mut session, smoke_mask(i))?;
    }
    match session.close()? {
        Some(finished) if finished == dir => Ok(dir),
        other => Err(format!(
            "expected {} to finish, got {other:?}",
            dir.display()
        )),
    }
}
