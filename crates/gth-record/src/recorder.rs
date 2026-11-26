//! [`Recorder`]: writes a recording while the caller runs the core through it.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gth_config::{CoreLock, GameConfig, RamVar};
use gth_core::Core;

use crate::format::{Check, INPUTS_MAGIC, Input};
use crate::meta::{ContentMeta, CoreMeta, FORMAT_VERSION, Meta, RecorderKind, StartMeta};
use crate::{Error, Result, content_identity, io_error, sha1_hex};

/// zstd level for every file: small enough output for tiny inputs, fast enough to compress a
/// megabyte start state at each episode start (docs/recording-format.md, "Compression").
pub(crate) const ZSTD_LEVEL: i32 = 9;

/// What the caller states about a recording, kept in `meta.toml`. Every field is required, so
/// who made a recording and under which license is never left to a default.
#[derive(Debug, Clone)]
pub struct RecordingInfo {
    pub game_id: String,
    pub recorder: RecorderKind,
    pub player: String,
    pub license: String,
    /// The harness build (`_native.build_info()`).
    pub harness: BTreeMap<String, String>,
    /// The core's `cores.lock` entry, if the caller has one.
    pub core_lock: Option<CoreLock>,
    /// Frames between two checks: smaller finds a desync closer to its cause and costs a
    /// frame and RAM hash each time (24 bytes plus 9 per RAM variable).
    pub checks_every: NonZeroU32,
}

type Encoder = zstd::Encoder<'static, BufWriter<File>>;

/// Writes a recording directory (`<id>.gthrec/`, docs/recording-format.md) from the state the
/// core is in when it is created. Run every frame of the recording through
/// [`run`](Self::run), then [`finish`](Self::finish), which writes `meta.toml`: a directory
/// without it is an incomplete recording that [`Recording::open`](crate::Recording::open)
/// refuses.
///
/// The recorder doesn't own the core, so the caller must not run it other than through
/// [`run`](Self::run) (nor load a state) until it has finished.
///
/// Any error in [`run`](Self::run) is terminal: the core may have run a frame the recording
/// doesn't hold, so later calls fail with [`Error::Failed`] and the directory stays
/// incomplete.
pub struct Recorder {
    dir: PathBuf,
    meta: Meta,
    ram: BTreeMap<String, RamVar>,
    inputs: Encoder,
    checks: Encoder,
    /// Scratch for one encoded check.
    check: Vec<u8>,
    /// A write failed: refuse to go on (see the type's docs).
    failed: bool,
    /// Makes the next input write fail, to test the above.
    #[cfg(test)]
    fail_next_write: bool,
}

fn encoder(path: &Path) -> Result<Encoder> {
    let file = File::create(path).map_err(io_error(path))?;
    zstd::Encoder::new(BufWriter::new(file), ZSTD_LEVEL).map_err(io_error(path))
}

impl Recorder {
    /// Starts a recording in the new directory `dir` (its parent is created if missing; `dir`
    /// itself must not exist), from the core's current state, which it reloads (as a replay
    /// does, so both start with no frame drawn). `game` is the config the core runs (copied
    /// into the recording), and `content` the path the core loaded, whose SHA-1 (or for an
    /// `in_parent` clone, its parent archive's) goes into `meta.toml`.
    pub fn create(
        dir: &Path,
        core: &mut Core,
        game: &GameConfig,
        content: Option<&Path>,
        info: RecordingInfo,
    ) -> Result<Recorder> {
        let mismatch = |reason: &str| Error::Mismatch {
            path: dir.to_owned(),
            reason: reason.to_owned(),
        };
        if info.game_id.is_empty() || info.player.is_empty() || info.license.is_empty() {
            return Err(mismatch("game_id, player and license must not be empty"));
        }
        let state = core.serialize()?;
        // The replay starts by loading this state, which clears the frame: a core that repeats
        // its frame (dupes) until the first check would otherwise hash the last frame drawn
        // before recording here and a black one there. Recording from the reloaded state also
        // finds a core whose savestates don't restore everything now rather than at replay.
        core.unserialize(&state)?;
        let content = match content {
            Some(path) => {
                let file = content_identity(game, path);
                Some(ContentMeta {
                    file: file
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    sha1: gth_config::sha1_file(&file)?,
                })
            }
            None => None,
        };
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(io_error(parent))?;
        }
        std::fs::create_dir(dir).map_err(io_error(dir))?;

        let game_toml = game.to_toml();
        let path = dir.join("game.toml");
        std::fs::write(&path, &game_toml).map_err(io_error(&path))?;
        let path = dir.join("start.state");
        let compressed = zstd::encode_all(state.as_slice(), ZSTD_LEVEL).map_err(io_error(&path))?;
        std::fs::write(&path, compressed).map_err(io_error(&path))?;

        let mut inputs = encoder(&dir.join("inputs.bin"))?;
        inputs
            .write_all(INPUTS_MAGIC)
            .map_err(io_error(&dir.join("inputs.bin")))?;
        let mut checks = encoder(&dir.join("checks.bin"))?;
        checks
            .write_all(&Check::header(game.ram.len()))
            .map_err(io_error(&dir.join("checks.bin")))?;

        let system = core.system_info();
        let meta = Meta {
            format_version: FORMAT_VERSION,
            game_id: info.game_id,
            game_config_sha1: sha1_hex(game_toml.as_bytes()),
            recorder: info.recorder,
            player: info.player,
            license: info.license,
            created_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            fps: core.av_info().fps,
            frames: 0,
            steps: 0,
            checks_every: info.checks_every.get(),
            core: CoreMeta {
                name: game.core.clone(),
                library_name: system.library_name.clone(),
                library_version: system.library_version.clone(),
                lock: info.core_lock.map(|lock| {
                    toml::Table::try_from(lock).expect("a CoreLock serializes to a table")
                }),
            },
            content,
            start: StartMeta {
                sha1: sha1_hex(&state),
                size: state.len() as u64,
            },
            harness: info.harness,
        };
        Ok(Recorder {
            dir: dir.to_owned(),
            meta,
            ram: game.ram.clone(),
            inputs,
            checks,
            check: Vec::new(),
            failed: false,
            #[cfg(test)]
            fail_next_write: false,
        })
    }

    /// The recording's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Frames recorded so far.
    pub fn frames(&self) -> u64 {
        self.meta.frames
    }

    /// Runs one agent step: `frames` frames with `buttons` held, as
    /// [`Core::run_frames`] does, recording each and checking every `checks_every` frames.
    /// `frames == 0` runs and records nothing.
    pub fn run(&mut self, core: &mut Core, frames: u32, buttons: u16) -> Result<()> {
        self.check_not_failed()?;
        let result = self.record(core, frames, buttons);
        self.failed = result.is_err();
        result
    }

    fn check_not_failed(&self) -> Result<()> {
        if self.failed {
            return Err(Error::Failed {
                path: self.dir.clone(),
            });
        }
        Ok(())
    }

    fn record(&mut self, core: &mut Core, frames: u32, buttons: u16) -> Result<()> {
        for i in 0..frames {
            core.run_frames(1, buttons);
            let input = Input {
                buttons,
                step_end: i + 1 == frames,
            };
            let path = self.dir.join("inputs.bin");
            #[cfg(test)]
            if std::mem::take(&mut self.fail_next_write) {
                return Err(io_error(&path)(std::io::Error::other("injected failure")));
            }
            self.inputs
                .write_all(&input.encode())
                .map_err(io_error(&path))?;
            self.meta.frames += 1;
            if self
                .meta
                .frames
                .is_multiple_of(u64::from(self.meta.checks_every))
            {
                self.write_check(core)?;
            }
        }
        if frames > 0 {
            self.meta.steps += 1;
        }
        Ok(())
    }

    fn write_check(&mut self, core: &mut Core) -> Result<()> {
        self.check.clear();
        Check::take(core, self.meta.frames, &self.ram).encode(&mut self.check);
        self.checks
            .write_all(&self.check)
            .map_err(io_error(&self.dir.join("checks.bin")))
    }

    /// Checks the state after the last frame (unless a periodic check just did), closes the
    /// files and writes `meta.toml`, which completes the recording. Returns its directory.
    /// Fails with [`Error::Failed`], writing nothing, if a [`run`](Self::run) failed.
    pub fn finish(mut self, core: &mut Core) -> Result<PathBuf> {
        self.check_not_failed()?;
        if !self
            .meta
            .frames
            .is_multiple_of(u64::from(self.meta.checks_every))
        {
            self.write_check(core)?;
        }
        for (encoder, name) in [(self.inputs, "inputs.bin"), (self.checks, "checks.bin")] {
            let path = self.dir.join(name);
            let mut writer = encoder.finish().map_err(io_error(&path))?;
            writer.flush().map_err(io_error(&path))?;
        }
        let path = self.dir.join("meta.toml");
        std::fs::write(&path, self.meta.to_toml()).map_err(io_error(&path))?;
        Ok(self.dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_write_leaves_the_recording_incomplete() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/testcore/game.toml");
        let game = GameConfig::load(&path).unwrap();
        let dir = std::env::temp_dir().join(format!("gth-record-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut core = Core::load(
            gth_test_support::testcore_path(),
            None,
            &std::env::temp_dir(),
            &game.core_options,
        )
        .unwrap();
        let info = RecordingInfo {
            game_id: "testcore".into(),
            recorder: RecorderKind::Scripted,
            player: "unit test".into(),
            license: "CC0-1.0".into(),
            harness: BTreeMap::new(),
            core_lock: None,
            checks_every: NonZeroU32::new(10).unwrap(),
        };
        let recording = dir.join("failed.gthrec");
        let mut recorder = Recorder::create(&recording, &mut core, &game, None, info).unwrap();
        recorder.run(&mut core, 5, 0).unwrap();
        recorder.fail_next_write = true;
        let error = recorder.run(&mut core, 5, 0).unwrap_err();
        assert!(error.to_string().contains("injected failure"), "{error}");
        let error = recorder.run(&mut core, 5, 0).unwrap_err();
        assert!(matches!(error, Error::Failed { .. }), "{error}");
        let error = recorder.finish(&mut core).unwrap_err();
        assert!(matches!(error, Error::Failed { .. }), "{error}");
        assert!(!recording.join("meta.toml").exists());
        let error = crate::Recording::open(&recording).unwrap_err();
        assert!(error.to_string().contains("incomplete"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
