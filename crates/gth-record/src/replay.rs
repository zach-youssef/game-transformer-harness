//! [`Recording`] (a recording read and validated) and [`Replayer`] (re-runs it on a core).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gth_config::GameConfig;
use gth_core::Core;

use crate::format::{Check, Input};
use crate::meta::Meta;
use crate::{Error, Result, content_identity, format_error, io_error, sha1_hex};

/// A complete recording, read into memory and checked for consistency: format version,
/// file hashes and sizes, and that its inputs and checks agree with `meta.toml`.
#[derive(Debug, Clone)]
pub struct Recording {
    pub dir: PathBuf,
    pub meta: Meta,
    /// The game config the recording was made with (its `game.toml`).
    pub game: GameConfig,
    pub start_state: Vec<u8>,
    pub inputs: Vec<Input>,
    pub checks: Vec<Check>,
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(io_error(path))
}

/// A stored `game.toml` from format `version` as today's gth-config reads it. Version 1
/// predates `settle_frames`: its recordings were made loading states as they are, which is
/// `settle_frames = 0` (a top-level key, so it goes before the first table).
fn migrate_game_toml(version: u32, text: String) -> String {
    if version == 1 {
        format!("settle_frames = 0\n{text}")
    } else {
        text
    }
}

fn decompress(path: &Path) -> Result<Vec<u8>> {
    zstd::decode_all(read(path)?.as_slice())
        .map_err(|error| format_error(path, format!("cannot decompress: {error}")))
}

impl Recording {
    /// Reads the recording in `dir`. Fails with [`Error::Format`] if it is incomplete (no
    /// `meta.toml`), from another format version, or inconsistent.
    pub fn open(dir: &Path) -> Result<Recording> {
        let bad = |reason: String| format_error(dir, reason);
        let meta_path = dir.join("meta.toml");
        if !meta_path.is_file() {
            return Err(bad(
                "no meta.toml: the recording is incomplete (never finished)".into(),
            ));
        }
        let text = String::from_utf8(read(&meta_path)?)
            .map_err(|_| bad("meta.toml isn't UTF-8".into()))?;
        let meta = Meta::parse(&text, dir)?;

        let game_toml = String::from_utf8(read(&dir.join("game.toml"))?)
            .map_err(|_| bad("game.toml isn't UTF-8".into()))?;
        if sha1_hex(game_toml.as_bytes()) != meta.game_config_sha1 {
            return Err(bad("game.toml doesn't have the SHA-1 in meta.toml".into()));
        }
        let game = GameConfig::parse(
            &migrate_game_toml(meta.format_version, game_toml),
            &dir.join("game.toml").display().to_string(),
        )?;

        let start_state = decompress(&dir.join("start.state"))?;
        if start_state.len() as u64 != meta.start.size || sha1_hex(&start_state) != meta.start.sha1
        {
            return Err(bad(
                "start.state doesn't have the size and SHA-1 in meta.toml".into(),
            ));
        }

        let inputs = Input::decode_all(&decompress(&dir.join("inputs.bin"))?, dir)?;
        let steps = inputs.iter().filter(|input| input.step_end).count() as u64;
        if inputs.len() as u64 != meta.frames || steps != meta.steps {
            return Err(bad(format!(
                "inputs.bin has {} frames in {steps} steps, meta.toml says {} in {}",
                inputs.len(),
                meta.frames,
                meta.steps
            )));
        }
        if inputs.last().is_some_and(|input| !input.step_end) {
            return Err(bad("inputs.bin ends inside a step".into()));
        }

        let checks = Check::decode_all(&decompress(&dir.join("checks.bin"))?, game.ram.len(), dir)?;
        if meta.checks_every == 0 {
            return Err(bad("checks_every is 0".into()));
        }
        let every = u64::from(meta.checks_every);
        // Check i follows frame (i + 1) * every, and a last one the last frame.
        let expected = (1..=meta.frames.div_ceil(every)).map(|i| (i * every).min(meta.frames));
        if !checks.iter().map(|check| check.frame).eq(expected) {
            return Err(bad(format!(
                "checks.bin's check frames aren't every {} of {} frames and the last",
                meta.checks_every, meta.frames
            )));
        }
        Ok(Recording {
            dir: dir.to_owned(),
            meta,
            game,
            start_state,
            inputs,
            checks,
        })
    }
}

/// Where [`Replayer::run_until_terminated`] stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    /// Frames replayed in all.
    pub frame: u64,
    /// The game's RAM variables after that frame, by name.
    pub ram: BTreeMap<String, i64>,
    /// Whether `episode.terminate` held there; false when the cap or the recording's end
    /// stopped the replay first.
    pub terminated: bool,
}

/// Re-runs a [`Recording`] on a core, step by step, comparing every check: the first one that
/// differs is an [`Error::Desync`] naming the frames since the previous check. The core is
/// the replayer's own, so nothing else can run it in between; render frames from
/// [`core`](Self::core).
pub struct Replayer {
    recording: Recording,
    core: Core,
    /// Frames replayed.
    frame: u64,
    /// Index of the next check to compare.
    next_check: usize,
}

impl Replayer {
    /// Loads the core at `core_path` with the recording's core options and the content, and
    /// replays from the start state. Fails with [`Error::Mismatch`] if the content (its SHA-1,
    /// or for an `in_parent` clone its parent archive's, or whether there is any) or the
    /// core's name and version differ from the recording's.
    pub fn load(
        recording: Recording,
        core_path: &Path,
        content_path: Option<&Path>,
        system_dir: &Path,
    ) -> Result<Replayer> {
        let mismatch = |reason: String| Error::Mismatch {
            path: recording.dir.clone(),
            reason,
        };
        match (&recording.meta.content, content_path) {
            (Some(expected), Some(path)) => {
                let file = content_identity(&recording.game, path);
                let sha1 = gth_config::sha1_file(&file)?;
                if sha1 != expected.sha1 {
                    return Err(mismatch(format!(
                        "content {} has SHA-1 {sha1}, the recording's {} has {}",
                        file.display(),
                        expected.file,
                        expected.sha1
                    )));
                }
            }
            (Some(expected), None) => {
                return Err(mismatch(format!(
                    "the recording was made with content {} (SHA-1 {}), and none was given",
                    expected.file, expected.sha1
                )));
            }
            (None, Some(path)) => {
                return Err(mismatch(format!(
                    "the recording was made without content, and {} was given",
                    path.display()
                )));
            }
            (None, None) => {}
        }
        let core = Core::load(
            core_path,
            content_path,
            system_dir,
            &recording.game.core_options,
            recording.game.settle_frames,
        )?;
        Replayer::new(recording, core)
    }

    /// Replays `recording` on `core`, which must have loaded the recording's game with its
    /// core options. Fails with [`Error::Mismatch`] if the core's name or version differ (a
    /// replay only reproduces on the same core build) or it settles after loads for another
    /// number of frames than the game's `settle_frames`, and with [`gth_core::Error::Ram`] if a
    /// RAM variable isn't in the core's memory.
    pub fn new(recording: Recording, mut core: Core) -> Result<Replayer> {
        let info = core.system_info();
        let recorded = &recording.meta.core;
        if info.library_name != recorded.library_name
            || info.library_version != recorded.library_version
        {
            return Err(Error::Mismatch {
                path: recording.dir.clone(),
                reason: format!(
                    "recorded with core {} {}, replayed with {} {}",
                    recorded.library_name,
                    recorded.library_version,
                    info.library_name,
                    info.library_version
                ),
            });
        }
        if core.settle_frames() != recording.game.settle_frames {
            return Err(Error::Mismatch {
                path: recording.dir.clone(),
                reason: format!(
                    "the core settles for {} frames after loads, the game for {}",
                    core.settle_frames(),
                    recording.game.settle_frames
                ),
            });
        }
        core.check_ram_vars(&recording.game.ram)?;
        core.unserialize(&recording.start_state)?;
        Ok(Replayer {
            recording,
            core,
            frame: 0,
            next_check: 0,
        })
    }

    pub fn recording(&self) -> &Recording {
        &self.recording
    }

    pub fn core(&self) -> &Core {
        &self.core
    }

    /// The core, for reads that need `&mut` (named areas, `gth_sprites`). Running frames or
    /// loading states through it would desync the replay, so it stays in the crate.
    pub(crate) fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    /// Frames replayed so far.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Replays the next agent step and returns its first frame's buttons (under sticky
    /// actions, a step's frames may differ) and its frame count, or `None` at the end of the
    /// recording.
    pub fn step(&mut self) -> Result<Option<(u16, u32)>> {
        let mut first = None;
        let mut frames = 0;
        while let Some(input) = self.run_frame()? {
            first.get_or_insert(input.buttons);
            frames += 1;
            if input.step_end {
                break;
            }
        }
        Ok(first.map(|buttons| (buttons, frames)))
    }

    /// Replays the next frame and returns its input, or `None` at the end of the recording.
    /// Step boundaries are the caller's to track (`Input::step_end`).
    pub fn run_frame(&mut self) -> Result<Option<Input>> {
        let Some(input) = self.recording.inputs.get(self.frame as usize).copied() else {
            return Ok(None);
        };
        self.core.run_frames(1, input.buttons);
        self.frame += 1;
        self.verify_check()?;
        Ok(Some(input))
    }

    /// Decodes the recording's RAM variables (its `game.toml`'s) from the core now, by name.
    /// For rewards, labels and validation, never a policy's input.
    pub fn read_ram_vars(&mut self) -> Result<BTreeMap<String, i64>> {
        Ok(self.core.read_ram_vars(&self.recording.game.ram)?)
    }

    /// Replays frame by frame, verifying checks, until `game`'s `episode.terminate` first
    /// holds after a frame, the replay has run `cap` frames in all, or the recording ends,
    /// whichever comes first. `game` gives the RAM variables and the condition: it may be
    /// another config of the same game than the recording's own (today's `games/<id>`), so
    /// its variables are checked against the core's memory first ([`gth_core::Error::Ram`]).
    /// For scoring a replayed episode (`gtharness.reference`), never a policy's input.
    pub fn run_until_terminated(&mut self, game: &GameConfig, cap: u64) -> Result<Stopped> {
        self.core.check_ram_vars(&game.ram)?;
        while self.frame < cap && self.run_frame()?.is_some() {
            let ram = self.core.read_ram_vars(&game.ram)?;
            if game.terminated(&ram)? {
                return Ok(Stopped {
                    frame: self.frame,
                    ram,
                    terminated: true,
                });
            }
        }
        Ok(Stopped {
            frame: self.frame,
            ram: self.core.read_ram_vars(&game.ram)?,
            terminated: false,
        })
    }

    /// Replays the rest of the recording; returns the number of frames replayed in all.
    pub fn run_to_end(&mut self) -> Result<u64> {
        while self.step()?.is_some() {}
        Ok(self.frame)
    }

    fn verify_check(&mut self) -> Result<()> {
        let Some(expected) = self.recording.checks.get(self.next_check) else {
            return Ok(());
        };
        if expected.frame != self.frame {
            return Ok(());
        }
        self.next_check += 1;
        let actual = Check::take(&mut self.core, self.frame, &self.recording.game.ram);
        let names = self.recording.game.ram.keys();
        let detail = names
            .zip(actual.ram.iter().zip(&expected.ram))
            .find(|(_, (a, e))| a != e)
            .map(|(name, (a, e))| format!("RAM variable {name} is {a:?}, recorded {e:?}"))
            .or_else(|| (actual.ram_hash != expected.ram_hash).then(|| "RAM hash differs".into()))
            .or_else(|| {
                (actual.frame_hash != expected.frame_hash).then(|| "frame hash differs".into())
            });
        match detail {
            Some(detail) => Err(Error::Desync {
                first: self
                    .next_check
                    .checked_sub(2)
                    .map_or(1, |previous| self.recording.checks[previous].frame + 1),
                frame: self.frame,
                detail,
            }),
            None => Ok(()),
        }
    }
}
