//! `gth-play`: play a game in a window and record it (docs/human-play.md).

mod app;
mod pads;
mod sound;

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::ExitCode;

use gth_config::RetroButton;
use gth_play::game::{self, GamePaths};
use gth_play::session::{RING_REWINDS, RecordSettings, RewindMode, Session};
use gth_record::RecorderKind;

/// The longest `--rewind-seconds`: the ring then holds 4 x 600 s of savestates, about
/// 380 MB on Gunhed (80 KB a state, docs/human-play.md, "Rewind").
const MAX_REWIND_SECONDS: f64 = 600.0;

/// `--rewind-seconds`: from 0 to [`MAX_REWIND_SECONDS`].
fn parse_rewind_seconds(text: &str) -> Result<f64, String> {
    text.parse::<f64>()
        .ok()
        .filter(|s| (0.0..=MAX_REWIND_SECONDS).contains(s))
        .ok_or(format!(
            "--rewind-seconds: a number of seconds from 0 to {MAX_REWIND_SECONDS} (the ring \
             buffer keeps {RING_REWINDS} rewinds of savestates in memory)"
        ))
}

/// The rewind length in whole frames at `fps`; a nonzero length under one frame is refused
/// rather than turning rewinds off.
fn rewind_frames(seconds: f64, fps: f64) -> Result<u64, String> {
    let frames = (seconds * fps).round() as u64;
    if seconds > 0.0 && frames == 0 {
        return Err(format!(
            "--rewind-seconds {seconds} is under one frame at {fps:.2} fps: give 0 for no \
             rewinds, or a longer rewind"
        ));
    }
    Ok(frames)
}

const USAGE: &str = "\
usage:
  gth-play GAME_ID --games-dir DIR --data-dir DIR --lock FILE --out DIR --player NAME
           --license SPDX --checks-every N --rewind-seconds S [--rom-dir DIR] [--core PATH]
           [--quit-after FRAMES]
      Play GAME_ID in a window; the record hotkey records to --out (docs/human-play.md).
      While recording, a death (the game's lives dropping) or the rewind hotkey rewinds S
      seconds (0: never, at most 600) and keeps the rewound frames as a clip.
  gth-play --verify RECORDING --data-dir DIR [--rom-dir DIR] [--core PATH]
      Replay a recording and fail on any desync.
  gth-play --measure-latency GAME_ID --games-dir DIR --data-dir DIR --state FILE
           --buttons B,... --frames N [--rom-dir DIR] [--core PATH]
      Frames until each button's press shows, from a savestate (docs/human-play.md).
  gth-play --headless-smoke --core PATH --games-dir DIR --out DIR
      Run the player's frame loop on the test core with scripted input; record 600 frames.";

/// Parsed `--name value` options after the positional argument.
struct Options {
    positional: Option<String>,
    values: Vec<(String, String)>,
}

impl Options {
    fn parse(args: &[String], allowed: &[&str]) -> Result<Options, String> {
        let mut options = Options {
            positional: None,
            values: Vec::new(),
        };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if let Some(name) = arg.strip_prefix("--") {
                if !allowed.contains(&name) {
                    return Err(format!("unknown option --{name}"));
                }
                if options.values.iter().any(|(n, _)| n == name) {
                    return Err(format!("--{name} given twice"));
                }
                let value = args.next().ok_or(format!("--{name} needs a value"))?;
                options.values.push((name.to_owned(), value.clone()));
            } else if options.positional.is_none() {
                options.positional = Some(arg.clone());
            } else {
                return Err(format!("unexpected argument {arg}"));
            }
        }
        Ok(options)
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn required(&self, name: &str) -> Result<&str, String> {
        self.get(name).ok_or(format!("--{name} is required"))
    }

    fn path(&self, name: &str) -> Result<PathBuf, String> {
        self.required(name).map(PathBuf::from)
    }

    fn positional(&self, what: &str) -> Result<&str, String> {
        self.positional
            .as_deref()
            .ok_or(format!("{what} is required"))
    }

    fn game_paths(&self) -> Result<GamePaths, String> {
        Ok(GamePaths {
            games_dir: self.get("games-dir").map(PathBuf::from).unwrap_or_default(),
            data_dir: self.path("data-dir")?,
            rom_dir: self.get("rom-dir").map(PathBuf::from),
            core: self.get("core").map(PathBuf::from),
        })
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("--headless-smoke") => smoke(&args[1..]),
        Some("--verify") => verify(&args[1..]),
        Some("--measure-latency") => measure_latency(&args[1..]),
        Some(_) => play(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("gth-play: {error}");
            ExitCode::FAILURE
        }
    }
}

fn smoke(args: &[String]) -> Result<(), String> {
    let options = Options::parse(args, &["core", "games-dir", "out"])?;
    let dir = gth_play::smoke::run(
        &options.path("core")?,
        &options.path("games-dir")?,
        &options.path("out")?,
    )?;
    println!("{}", dir.display());
    Ok(())
}

fn verify(args: &[String]) -> Result<(), String> {
    let options = Options::parse(args, &["data-dir", "rom-dir", "core"])?;
    let dir = PathBuf::from(options.positional("RECORDING")?);
    let frames = game::verify(&dir, &options.game_paths()?)?;
    println!("{}: {frames} frames replayed, no desync", dir.display());
    Ok(())
}

fn measure_latency(args: &[String]) -> Result<(), String> {
    let options = Options::parse(
        args,
        &[
            "games-dir",
            "data-dir",
            "rom-dir",
            "core",
            "state",
            "buttons",
            "frames",
        ],
    )?;
    let game_id = options.positional("GAME_ID")?;
    options.required("games-dir")?;
    let mut loaded = game::load(game_id, &options.game_paths()?)?;
    let state_path = options.path("state")?;
    let state = std::fs::read(&state_path)
        .map_err(|e| format!("cannot read {}: {e}", state_path.display()))?;
    let frames: u32 = options
        .required("frames")?
        .parse()
        .map_err(|e| format!("--frames: {e}"))?;
    for name in options.required("buttons")?.split(',') {
        let button: RetroButton = toml::Value::String(name.trim().to_owned())
            .try_into()
            .map_err(|_| format!("--buttons: unknown RetroPad button {name:?}"))?;
        match gth_play::latency::frames_to_response(&mut loaded.core, &state, button.bit(), frames)?
        {
            Some(n) => println!("{name}: shows on frame {n} after the press"),
            None => println!("{name}: no change within {frames} frames"),
        }
    }
    Ok(())
}

fn play(args: &[String]) -> Result<(), String> {
    let options = Options::parse(
        args,
        &[
            "games-dir",
            "data-dir",
            "rom-dir",
            "core",
            "lock",
            "out",
            "player",
            "license",
            "checks-every",
            "rewind-seconds",
            "quit-after",
        ],
    )?;
    let game_id = options.positional("GAME_ID")?;
    let quit_after = options
        .get("quit-after")
        .map(|n| n.parse::<u64>().map_err(|e| format!("--quit-after: {e}")))
        .transpose()?;
    options.required("games-dir")?;
    let checks_every = options
        .required("checks-every")?
        .parse::<NonZeroU32>()
        .map_err(|e| format!("--checks-every: {e}"))?;
    let rewind_seconds = parse_rewind_seconds(options.required("rewind-seconds")?)?;
    let lock_path = options.path("lock")?;
    let (dir, player, license) = (
        options.path("out")?,
        options.required("player")?.to_owned(),
        options.required("license")?.to_owned(),
    );
    let loaded = game::load(game_id, &options.game_paths()?)?;
    eprintln!(
        "gth-play: {game_id} on {} ({})",
        loaded.core.system_info().library_name,
        loaded.core_path.display()
    );
    let core_lock = game::pinned_core(&lock_path, &loaded.game, &loaded.core_path)?;
    match &core_lock {
        Some(lock) => eprintln!(
            "gth-play: the core is the build {} pins (commit {}): recordings keep [core.lock]",
            lock_path.display(),
            lock.commit
        ),
        None => eprintln!(
            "gth-play: warning: {} isn't the {} build of {} that {} pins: recordings won't \
             keep [core.lock] (docs/cores.md, \"Fetching\")",
            loaded.core_path.display(),
            gth_build_info::build_info()["target"],
            loaded.game.core,
            lock_path.display()
        ),
    }
    let fps = loaded.core.av_info().fps;
    let rewind_frames = rewind_frames(rewind_seconds, fps)?;
    let settings = RecordSettings {
        dir,
        recorder: RecorderKind::Human,
        player,
        license,
        checks_every,
        core_lock,
        rewind_frames,
    };
    let screen = gth_play::display::Screen::new(&loaded.game);
    let mut session = Session::new(
        loaded.core,
        loaded.game,
        game_id,
        Some(loaded.content),
        settings,
    );
    if session.rewind_mode() != RewindMode::Off {
        let depth = session.ring_depth();
        match session.core_mut().serialize() {
            Ok(state) => eprintln!(
                "gth-play: rewind ring: up to {depth} savestates of {} KB, about {} MB",
                state.len().div_ceil(1000),
                (depth * state.len()).div_ceil(1_000_000)
            ),
            Err(error) => eprintln!("gth-play: warning: no savestates, so no rewinds: {error}"),
        }
    }
    match session.rewind_mode() {
        RewindMode::Off => eprintln!("gth-play: rewind off (--rewind-seconds 0)"),
        RewindMode::ManualOnly(why) => eprintln!(
            "gth-play: rewind: MANUAL ONLY, {rewind_frames} frames on the rewind hotkey; no \
             automatic rewind on a death: {why}"
        ),
        RewindMode::Auto => eprintln!(
            "gth-play: rewind: {rewind_frames} frames on a death (`lives` drops) or the \
             rewind hotkey"
        ),
    }
    app::run(session, screen, game_id, quit_after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rewind_length_is_bounded_and_at_least_a_frame() {
        assert_eq!(parse_rewind_seconds("3"), Ok(3.0));
        assert_eq!(parse_rewind_seconds("600"), Ok(600.0));
        for bad in ["600.5", "1e17", "-1", "inf", "NaN", "three"] {
            let error = parse_rewind_seconds(bad).unwrap_err();
            assert!(error.contains("from 0 to 600"), "{bad}: {error}");
        }
        assert_eq!(rewind_frames(3.0, 60.0), Ok(180));
        assert_eq!(rewind_frames(0.0, 60.0), Ok(0));
        assert_eq!(rewind_frames(600.0, 59.82), Ok(35892));
        let error = rewind_frames(0.004, 60.0).unwrap_err();
        assert!(error.contains("under one frame"), "{error}");
    }
}
