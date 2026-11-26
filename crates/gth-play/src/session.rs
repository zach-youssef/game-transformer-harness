//! [`Session`]: the headless half of the player. It runs the core one frame at a time,
//! records through `gth-record`, and handles the hotkeys (save and load state, pause,
//! recording). The window, audio and controllers (the binary) only feed it masks.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gth_config::GameConfig;
use gth_core::Core;
use gth_record::{Recorder, RecorderKind, RecordingInfo};

/// How and where recordings are made. Every field is required (as in `RecordingInfo`), so
/// who recorded and under which license is never a default.
#[derive(Debug, Clone)]
pub struct RecordSettings {
    /// Recordings go to `<dir>/<game_id>-<UTC time>-<pid>-<n>.gthrec`, as `RetroEnv`'s.
    pub dir: PathBuf,
    pub recorder: RecorderKind,
    pub player: String,
    pub license: String,
    pub checks_every: NonZeroU32,
}

/// What a hotkey did, for the player to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Paused(bool),
    StateSaved(PathBuf),
    StateLoaded,
    RecordingStarted(PathBuf),
    RecordingFinished(PathBuf),
}

/// One game being played.
pub struct Session {
    core: Core,
    game: GameConfig,
    game_id: String,
    content: Option<PathBuf>,
    settings: RecordSettings,
    recorder: Option<Recorder>,
    /// The practice slot, also kept in `<dir>/<game_id>.state`.
    slot: Option<Vec<u8>>,
    paused: bool,
    /// Recordings started by this session, to number their names.
    started: u32,
}

/// Errors are strings: the player shows them and carries on or quits.
pub type Result<T> = std::result::Result<T, String>;

impl Session {
    /// `core` has loaded `game` (`content` is the path it loaded, for the recording's
    /// identity).
    pub fn new(
        core: Core,
        game: GameConfig,
        game_id: &str,
        content: Option<PathBuf>,
        settings: RecordSettings,
    ) -> Session {
        Session {
            core,
            game,
            game_id: game_id.to_owned(),
            content,
            settings,
            recorder: None,
            slot: None,
            paused: false,
            started: 0,
        }
    }

    pub fn core(&self) -> &Core {
        &self.core
    }

    /// The core, to take its audio and frame. Don't run it or load states through this
    /// while recording: the recording would miss those frames.
    pub fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// The running recording's directory and frames so far.
    pub fn recording(&self) -> Option<(&Path, u64)> {
        self.recorder.as_ref().map(|r| (r.dir(), r.frames()))
    }

    /// Runs one frame with `buttons` held (recorded, when recording), unless paused.
    /// Returns whether a frame ran. A failed recording write ends the recording, which
    /// stays incomplete, and is returned as the error; play goes on.
    pub fn run_frame(&mut self, buttons: u16) -> Result<bool> {
        if self.paused {
            return Ok(false);
        }
        match &mut self.recorder {
            Some(recorder) => {
                if let Err(error) = recorder.run(&mut self.core, 1, buttons) {
                    self.recorder = None;
                    return Err(format!("recording failed and stays incomplete: {error}"));
                }
            }
            None => self.core.run_frames(1, buttons),
        }
        Ok(true)
    }

    pub fn toggle_pause(&mut self) -> Event {
        self.paused = !self.paused;
        Event::Paused(self.paused)
    }

    /// The practice slot's file.
    pub fn state_path(&self) -> PathBuf {
        self.settings.dir.join(format!("{}.state", self.game_id))
    }

    /// Saves the state into the slot and its file (so it outlives the session and
    /// `gth-play --measure-latency` can use it). A recording goes on.
    pub fn save_state(&mut self) -> Result<Event> {
        let state = self.core.serialize().map_err(|e| e.to_string())?;
        let path = self.state_path();
        std::fs::create_dir_all(&self.settings.dir)
            .and_then(|()| std::fs::write(&path, &state))
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        self.slot = Some(state);
        Ok(Event::StateSaved(path))
    }

    /// Loads the slot, or the slot's file from an earlier session. A recording can't hold a
    /// state load, so a running one is finished first (its events come first).
    pub fn load_state(&mut self) -> Result<Vec<Event>> {
        let state = match &self.slot {
            Some(state) => state.clone(),
            None => {
                let path = self.state_path();
                std::fs::read(&path)
                    .map_err(|error| format!("no state saved yet ({}: {error})", path.display()))?
            }
        };
        let mut events = Vec::new();
        if self.recorder.is_some() {
            events.push(self.toggle_recording()?);
        }
        self.core.unserialize(&state).map_err(|e| e.to_string())?;
        self.slot = Some(state);
        events.push(Event::StateLoaded);
        Ok(events)
    }

    /// Starts a recording from the current state (written into it), or finishes the
    /// running one.
    pub fn toggle_recording(&mut self) -> Result<Event> {
        if let Some(recorder) = self.recorder.take() {
            let dir = recorder
                .finish(&mut self.core)
                .map_err(|error| format!("cannot finish the recording: {error}"))?;
            return Ok(Event::RecordingFinished(dir));
        }
        let name = recording_name(&self.game_id, unix_now(), self.started);
        let dir = self.settings.dir.join(name);
        let info = RecordingInfo {
            game_id: self.game_id.clone(),
            recorder: self.settings.recorder,
            player: self.settings.player.clone(),
            license: self.settings.license.clone(),
            harness: build_info(),
            core_lock: None,
            checks_every: self.settings.checks_every,
        };
        let recorder = Recorder::create(
            &dir,
            &mut self.core,
            &self.game,
            self.content.as_deref(),
            info,
        )
        .map_err(|error| format!("cannot start a recording: {error}"))?;
        self.started += 1;
        self.recorder = Some(recorder);
        Ok(Event::RecordingStarted(dir))
    }

    /// Finishes a running recording, returning it. Call before dropping the session: a
    /// dropped recorder leaves its recording incomplete.
    pub fn close(&mut self) -> Result<Option<PathBuf>> {
        match self.recorder {
            Some(_) => match self.toggle_recording()? {
                Event::RecordingFinished(dir) => Ok(Some(dir)),
                _ => unreachable!("a running recording finishes"),
            },
            None => Ok(None),
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `<game_id>-<YYYYmmddTHHMMSSZ>-<pid>-<nnnn>.gthrec`, as `RetroEnv` names its recordings.
pub fn recording_name(game_id: &str, unix_seconds: u64, number: u32) -> String {
    format!(
        "{game_id}-{}-{}-{number:04}.gthrec",
        utc_stamp(unix_seconds),
        std::process::id()
    )
}

/// `YYYYmmddTHHMMSSZ` for a time in seconds since the Unix epoch.
pub fn utc_stamp(unix_seconds: u64) -> String {
    let (days, seconds) = (unix_seconds / 86_400, unix_seconds % 86_400);
    // Howard Hinnant's days_from_civil, inverted (civil_from_days), for days >= 0.
    let z = days as i64 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// The build recorded in `meta.toml`'s `[harness]`, with the keys of `_native.build_info()`
/// (the build script is gth-py's). This repeats gth-py's `build_info` key list, which
/// `build_info_has_the_native_keys` pins; folding both into one shared function is a
/// follow-up (it touches gth-py's lib.rs).
pub fn build_info() -> BTreeMap<String, String> {
    [
        ("version", env!("CARGO_PKG_VERSION")),
        ("git_sha", env!("GTH_GIT_SHA")),
        ("git_dirty", env!("GTH_GIT_DIRTY")),
        ("target", env!("GTH_TARGET")),
        ("profile", env!("GTH_PROFILE")),
        ("rustc", env!("GTH_RUSTC")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_stamps() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(951_782_400), "20000229T000000Z");
        assert_eq!(utc_stamp(1_795_689_296), "20261126T103456Z");
        assert_eq!(utc_stamp(4_107_542_399), "21000228T235959Z");
    }

    #[test]
    fn recording_names_follow_retroenv() {
        let name = recording_name("gunhed", 0, 7);
        assert_eq!(
            name,
            format!("gunhed-19700101T000000Z-{}-0007.gthrec", std::process::id())
        );
    }

    #[test]
    fn build_info_has_the_native_keys() {
        let info = build_info();
        let keys: Vec<_> = info.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "git_dirty",
                "git_sha",
                "profile",
                "rustc",
                "target",
                "version"
            ]
        );
    }
}
