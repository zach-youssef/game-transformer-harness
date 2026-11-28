//! The recording format (docs/recording-format.md): a recording is a replay, the start
//! savestate plus the joypad mask of every emulated frame, with periodic hashes and RAM
//! values to detect desyncs. Frames aren't stored: [`Replayer`] re-runs the core to render
//! them, which needs the same core build the recording was made with. [`export`] renders a
//! recording into a training shard, and [`write_dataset`] writes one from an offline dataset's
//! arrays.

mod format;
mod meta;
mod recorder;
mod replay;
mod shard;

use std::path::{Path, PathBuf};

pub use format::{Check, Input};
pub use meta::{
    BranchMeta, ContentMeta, CoreMeta, FORMAT_VERSION, Meta, OLDEST_FORMAT_VERSION, RecorderKind,
    RewindCause, RewindMeta, StartMeta,
};
pub use recorder::{Recorder, RecordingInfo};
pub use replay::{Recording, Replayer, Stopped};
pub use shard::{
    DatasetMeta, DatasetSamples, FRAME_ZSTD_LEVEL, KeepAll, Playing, SHARD_VERSION,
    SPRITE_CHUNK_FRAMES, SampleFilter, ShardMeta, ShardOptions, ShardSummary, StepInfo, Steps,
    chunk_name, decode_frames, decode_sprites, export, filter_by_name, sprite_chunk_name,
    write_dataset,
};

/// Everything that can go wrong writing, reading or replaying a recording. Not
/// `#[non_exhaustive]`: `gth-py` maps every variant to a Python exception.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A recording file couldn't be written or read.
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A recording is incomplete, corrupt or in a format version this build doesn't read.
    #[error("invalid recording {path}: {reason}")]
    Format { path: PathBuf, reason: String },
    /// A recording doesn't fit what it is replayed with (another core build or content), or
    /// what it is written with is invalid.
    #[error("recording {path} doesn't match: {reason}")]
    Mismatch { path: PathBuf, reason: String },
    /// A [`Recorder`] failed to write earlier, so it refuses to go on: the recording stays
    /// incomplete (no `meta.toml`) rather than completing with frames missing.
    #[error("recording {path} failed earlier and stays incomplete")]
    Failed { path: PathBuf },
    /// A replay produced a different state than its recording at the check after `frame`.
    /// The first frame that differs is in `first..=frame` (`first` is the frame after the
    /// previous check), so it is `frame` itself when checks run every frame.
    #[error("{}", desync_message(*first, *frame, detail))]
    Desync {
        first: u64,
        frame: u64,
        detail: String,
    },
    #[error(transparent)]
    Core(#[from] gth_core::Error),
    #[error(transparent)]
    Config(#[from] gth_config::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_owned(),
        source,
    }
}

fn desync_message(first: u64, frame: u64, detail: &str) -> String {
    if first == frame {
        format!("replay desynced at frame {frame}: {detail}")
    } else {
        format!("replay desynced between frames {first} and {frame}: {detail}")
    }
}

fn format_error(path: &Path, reason: impl Into<String>) -> Error {
    Error::Format {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

/// The file whose SHA-1 identifies the content the core loads from `content`: that file, or
/// for an `in_parent` clone, whose path doesn't exist, the parent archive that holds it
/// (gth-config's `FileSpec::in_parent`). The archive's other sets aren't told apart.
fn content_identity(game: &gth_config::GameConfig, content: &Path) -> PathBuf {
    match game.content.parents.first() {
        Some(parent) if game.content.in_parent => content.with_file_name(&parent.file),
        _ => content.to_owned(),
    }
}

/// SHA-1 as 40 lowercase hex digits.
fn sha1_hex(data: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_cause() {
        let desync = |first| Error::Desync {
            first,
            frame: 42,
            detail: "RAM hash differs".into(),
        };
        assert_eq!(
            desync(42).to_string(),
            "replay desynced at frame 42: RAM hash differs"
        );
        assert_eq!(
            desync(31).to_string(),
            "replay desynced between frames 31 and 42: RAM hash differs"
        );
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn an_in_parent_clone_is_identified_by_its_parent_archive() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/testcore/game.toml");
        let mut game = gth_config::GameConfig::load(&path).unwrap();
        let content = Path::new("roms").join("clone.zip");
        assert_eq!(content_identity(&game, &content), content);
        game.content.in_parent = true;
        assert_eq!(
            content_identity(&game, &content),
            Path::new("roms").join("gth_testcore_parent.zip")
        );
    }
}
