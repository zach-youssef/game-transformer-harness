//! [`Session`]: the headless half of the player. It runs the core one frame at a time,
//! records through `gth-record`, and handles the hotkeys (save and load state, pause,
//! recording, rewind). The window, audio and controllers (the binary) only feed it masks.

use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gth_config::{CoreLock, GameConfig, RamVar};
use gth_core::Core;
use gth_record::{BranchMeta, Recorder, RecorderKind, RecordingInfo, RewindCause, RewindMeta};

/// Frames between two ring-buffer savestates while recording. A rewind loads the latest
/// one at or before its frame and replays the input log forward, so this bounds that replay
/// (docs/human-play.md, "Rewind", has the measurements behind it).
pub const SNAPSHOT_EVERY: u64 = 30;

/// Rewinds the ring buffer covers back to back: two deaths inside one rewind length of each
/// other rewind past the first one's branch frame.
pub const RING_REWINDS: u64 = 4;

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
    /// The core's `cores.lock` entry, kept as `[core.lock]`: only when the loaded core file
    /// is the build the lock pins (`game::pinned_core`), as `RetroEnv` does, else `None`.
    pub core_lock: Option<CoreLock>,
    /// Frames a rewind goes back (`--rewind-seconds`), 0 for no rewinds. It decides which
    /// frames the main recording keeps and where each clip starts, so it is required too.
    pub rewind_frames: u64,
}

/// What rewinds this session does (docs/human-play.md, "Rewind").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindMode {
    /// `rewind_frames` is 0.
    Off,
    /// The rewind hotkey only, with the reason there is no automatic rewind.
    ManualOnly(String),
    /// On a drop of the game's `lives` variable, and on the hotkey.
    Auto,
}

/// What a hotkey (or a rewind) did, for the player to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Paused(bool),
    StateSaved(PathBuf),
    StateLoaded,
    RecordingStarted(PathBuf),
    RecordingFinished(PathBuf),
    /// The recording was rewound from frame `from` to `to` (and the session paused); `clip`
    /// holds the frames in between.
    Rewound {
        from: u64,
        to: u64,
        cause: RewindCause,
        clip: PathBuf,
    },
    /// Something failed during a frame that play goes on through (an automatic rewind, or a
    /// ring savestate); the message says what, and whether the recording stopped.
    Warning(String),
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
    /// The game's `lives` variable, when the core's memory holds it.
    lives: Option<RamVar>,
    /// Why there is no `lives` to watch, when there isn't.
    no_lives: Option<String>,
    /// The running recording's rewind state.
    rewind: Option<Rewinding>,
    /// Events of automatic rewinds, for [`take_events`](Session::take_events).
    events: Vec<Event>,
}

/// Rewind state of the running recording.
struct Rewinding {
    /// Savestates `(frames, state)` of the recording, oldest first, one every
    /// [`SNAPSHOT_EVERY`] frames and one at its start.
    ring: VecDeque<(u64, Vec<u8>)>,
    /// The `lives` value after the last frame, when watched and readable.
    lives: Option<i64>,
    /// Clips made from the recording, to number their names.
    clips: u32,
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
        let mut core = core;
        let (lives, no_lives) = match game.ram.get("lives") {
            None => (
                None,
                Some("the game has no `lives` RAM variable".to_owned()),
            ),
            Some(var) => {
                let one = BTreeMap::from([("lives".to_owned(), *var)]);
                match core.check_ram_vars(&one) {
                    Ok(()) => (Some(*var), None),
                    Err(error) => (None, Some(format!("cannot read `lives`: {error}"))),
                }
            }
        };
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
            lives,
            no_lives,
            rewind: None,
            events: Vec::new(),
        }
    }

    /// What rewinds this session does, for the player to say at start.
    pub fn rewind_mode(&self) -> RewindMode {
        match (&self.no_lives, self.settings.rewind_frames) {
            (_, 0) => RewindMode::Off,
            (Some(why), _) => RewindMode::ManualOnly(why.clone()),
            (None, _) => RewindMode::Auto,
        }
    }

    /// The events of automatic rewinds since the last call, oldest first.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
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

    /// Clips made from the running recording so far (0 when not recording).
    pub fn rewinds(&self) -> u32 {
        self.rewind.as_ref().map_or(0, |r| r.clips)
    }

    /// The running recording's directory and frames so far.
    pub fn recording(&self) -> Option<(&Path, u64)> {
        self.recorder.as_ref().map(|r| (r.dir(), r.frames()))
    }

    /// Runs one frame with `buttons` held (recorded, when recording), unless paused.
    /// Returns whether a frame ran. A failed recording write ends the recording, which
    /// stays incomplete, and is returned as the error; play goes on.
    ///
    /// While recording, a drop of `lives` after the frame rewinds (see
    /// [`rewind`](Self::rewind)); its event waits in [`take_events`](Self::take_events), as
    /// does an [`Event::Warning`] if the rewind or a ring savestate fails: the frame has run
    /// either way. Only a failed recording write is the error.
    pub fn run_frame(&mut self, buttons: u16) -> Result<bool> {
        if self.paused {
            return Ok(false);
        }
        let Some(recorder) = &mut self.recorder else {
            self.core.run_frames(1, buttons);
            return Ok(true);
        };
        if let Err(error) = recorder.run(&mut self.core, 1, buttons) {
            self.stop_recording();
            return Err(format!("recording failed and stays incomplete: {error}"));
        }
        let frames = recorder.frames();
        if self.settings.rewind_frames == 0 {
            return Ok(true);
        }
        // Lives first, so a failed savestate can't delay a death by a frame.
        let lives = self.read_lives();
        let rewinding = self.rewinding();
        let dropped = matches!((rewinding.lives, lives), (Some(was), Some(now)) if now < was);
        rewinding.lives = lives;
        if dropped {
            match self.rewind_by(RewindCause::Lives) {
                // The core is back at the branch: no savestate of the death frame.
                Ok(event) => {
                    self.events.push(event);
                    return Ok(true);
                }
                Err(error) => self.events.push(Event::Warning(error)),
            }
        }
        if self.rewind.is_some() && frames.is_multiple_of(SNAPSHOT_EVERY) {
            match self.core.serialize() {
                Ok(state) => {
                    let depth = self.ring_depth();
                    let rewinding = self.rewinding();
                    rewinding.ring.push_back((frames, state));
                    while rewinding.ring.len() > depth {
                        rewinding.ring.pop_front();
                    }
                }
                Err(error) => self.events.push(Event::Warning(format!(
                    "no rewind savestate at frame {frames}, rewinds may reach less far: {error}"
                ))),
            }
        }
        Ok(true)
    }

    /// Savestates the ring keeps: [`RING_REWINDS`] rewinds back, plus one for the savestate
    /// before the oldest branch frame.
    pub fn ring_depth(&self) -> usize {
        let frames = RING_REWINDS.saturating_mul(self.settings.rewind_frames);
        usize::try_from(frames.div_ceil(SNAPSHOT_EVERY))
            .unwrap_or(usize::MAX)
            .saturating_add(1)
    }

    fn rewinding(&mut self) -> &mut Rewinding {
        self.rewind
            .as_mut()
            .expect("a recording with rewinds on has its rewind state")
    }

    /// `lives` now, when watched and its bytes decode.
    fn read_lives(&mut self) -> Option<i64> {
        let var = self.lives.as_ref()?;
        self.core.read_ram("lives", var).ok()
    }

    /// Ends the running recording without finishing it (it stays incomplete).
    fn stop_recording(&mut self) {
        self.recorder = None;
        self.rewind = None;
    }

    /// Rewinds the running recording by `rewind_frames` (to its start, or to just after the
    /// ring buffer's oldest savestate, if that's later), and pauses so the player can take over
    /// from there. The frames rewound over become a clip, a side recording labelled with
    /// the branch (`[branch]`, docs/recording-format.md), and the recording itself is
    /// truncated to the branch frame (`[[rewind]]`), so it stays one run from its start.
    ///
    /// Fails with nothing changed when not recording, rewinds are off, or there is nothing
    /// to rewind. If making the clip fails, the game goes back to where it was and the clip
    /// is removed. If the recording itself can't be rewritten, it stays incomplete.
    pub fn rewind(&mut self) -> Result<Event> {
        self.rewind_by(RewindCause::Manual)
    }

    fn rewind_by(&mut self, cause: RewindCause) -> Result<Event> {
        let Some(recorder) = &self.recorder else {
            return Err("nothing to rewind: not recording".into());
        };
        if self.settings.rewind_frames == 0 {
            return Err("rewind is off (--rewind-seconds 0)".into());
        }
        let death = recorder.frames();
        let oldest = self
            .rewinding()
            .ring
            .front()
            .map_or(death, |(frame, _)| *frame);
        // Past frame 0, one frame after the oldest savestate: `seek` must replay a frame to
        // land on the picture there (a loaded savestate has none, docs/human-play.md).
        let reachable = if oldest == 0 { 0 } else { oldest + 1 };
        let branch = death
            .saturating_sub(self.settings.rewind_frames)
            .max(reachable);
        if branch >= death {
            return Err(format!(
                "nothing to rewind: at frame {death}, the oldest reachable"
            ));
        }
        let clip = match self.make_clip(branch, death, cause) {
            Ok(clip) => clip,
            Err(error) => {
                let restored = self.seek(death);
                // The replays' samples aren't the player's to hear.
                self.core.take_audio(&mut Vec::new());
                if let Err(restore) = restored {
                    self.stop_recording();
                    return Err(format!(
                        "rewind failed ({error}) and so did going back ({restore}): the \
                         recording stops and stays incomplete"
                    ));
                }
                return Err(format!("rewind failed, play goes on: {error}"));
            }
        };
        let clip_name = file_name(&clip);
        let rewound = self.seek(branch).and_then(|()| {
            let recorder = self.recorder.as_mut().expect("checked above");
            recorder
                .rewind(RewindMeta {
                    frame: branch,
                    cause,
                    clip: clip_name,
                })
                .map_err(|e| e.to_string())
        });
        if let Err(error) = rewound {
            self.stop_recording();
            return Err(format!(
                "cannot rewind the recording, which stops and stays incomplete: {error}"
            ));
        }
        let lives = self.read_lives();
        let rewinding = self.rewinding();
        rewinding.ring.retain(|(frame, _)| *frame <= branch);
        rewinding.lives = lives;
        // The replays' samples aren't the player's to hear.
        self.core.take_audio(&mut Vec::new());
        self.paused = true;
        Ok(Event::Rewound {
            from: death,
            to: branch,
            cause,
            clip,
        })
    }

    /// Puts the core in the recording's state after `frame` frames: loads the ring's latest
    /// savestate before it and replays the logged inputs from there. Before, not at, so the
    /// core has drawn the frame shown at `frame` (a loaded savestate clears the picture);
    /// only frame 0 loads its own savestate, as the replayer starts with no picture too.
    fn seek(&mut self, frame: u64) -> Result<()> {
        let rewinding = self.rewind.as_ref().expect("seeking while rewinding");
        let (from, state) = rewinding
            .ring
            .iter()
            .rev()
            .find(|(at, _)| *at < frame || *at == 0 && frame == 0)
            .ok_or_else(|| format!("no savestate before frame {frame}"))?;
        self.core.unserialize(state).map_err(|e| e.to_string())?;
        let recorder = self.recorder.as_ref().expect("seeking while recording");
        for input in &recorder.inputs()[*from as usize..frame as usize] {
            self.core.run_frames(1, input.buttons);
        }
        Ok(())
    }

    /// Records frames `branch..death` of the running recording again, from the state at
    /// `branch`, as a clip beside it; returns the clip's directory. Leaves the core at
    /// `death` on success.
    fn make_clip(&mut self, branch: u64, death: u64, cause: RewindCause) -> Result<PathBuf> {
        self.seek(branch)?;
        let main = self
            .recorder
            .as_ref()
            .expect("making a clip while recording");
        let parent = file_name(main.dir());
        let stem = parent.strip_suffix(".gthrec").unwrap_or(&parent);
        let rewinding = self.rewind.as_mut().expect("making a clip while rewinding");
        rewinding.clips += 1;
        let dir = main
            .dir()
            .with_file_name(format!("{stem}-rewind-{:03}.gthrec", rewinding.clips));
        // Steps of the logged frames: each runs one mask (`Recorder::run`).
        let mut steps: Vec<(u32, u16)> = Vec::new();
        let mut open = false;
        for input in &main.inputs()[branch as usize..death as usize] {
            match steps.last_mut() {
                Some((frames, _)) if open => *frames += 1,
                _ => steps.push((1, input.buttons)),
            }
            open = !input.step_end;
        }
        let info = self.recording_info();
        let cannot =
            |error: gth_record::Error| format!("cannot record the clip {}: {error}", dir.display());
        // A failed create made nothing to remove (and `dir` may be someone else's).
        let mut clip = Recorder::create(
            &dir,
            &mut self.core,
            &self.game,
            self.content.as_deref(),
            info,
        )
        .map_err(cannot)?;
        let made = (|| {
            clip.set_branch(BranchMeta {
                parent: parent.clone(),
                frame: branch,
                cause,
            });
            for &(frames, buttons) in &steps {
                clip.run(&mut self.core, frames, buttons)?;
            }
            clip.finish(&mut self.core)
        })();
        made.map_err(|error| {
            let _ = std::fs::remove_dir_all(&dir);
            cannot(error)
        })
    }

    fn recording_info(&self) -> RecordingInfo {
        RecordingInfo {
            game_id: self.game_id.clone(),
            recorder: self.settings.recorder,
            player: self.settings.player.clone(),
            license: self.settings.license.clone(),
            harness: gth_build_info::build_info_owned(),
            core_lock: self.settings.core_lock.clone(),
            checks_every: self.settings.checks_every,
        }
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
            self.rewind = None;
            let dir = recorder
                .finish(&mut self.core)
                .map_err(|error| format!("cannot finish the recording: {error}"))?;
            return Ok(Event::RecordingFinished(dir));
        }
        let name = recording_name(&self.game_id, unix_now(), self.started);
        let dir = self.settings.dir.join(name);
        let info = self.recording_info();
        let recorder = Recorder::create(
            &dir,
            &mut self.core,
            &self.game,
            self.content.as_deref(),
            info,
        )
        .map_err(|error| format!("cannot start a recording: {error}"))?;
        self.started += 1;
        if self.settings.rewind_frames > 0 {
            // `create` reloaded the state it wrote, so this is the recording's start state.
            let state = self
                .core
                .serialize()
                .map_err(|error| format!("cannot snapshot the start for rewinds: {error}"))?;
            let lives = self.read_lives();
            self.rewind = Some(Rewinding {
                ring: VecDeque::from([(0, state)]),
                lives,
                clips: 0,
            });
        }
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

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
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
}
