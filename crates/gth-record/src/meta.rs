//! `meta.toml`: what a recording is of and how it was made, kept with it forever.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Result, format_error};

/// The recording format version this build writes and reads. Bump it when any file's layout
/// or meaning changes; [`Recording::open`](crate::Recording::open) refuses other versions.
/// The stored `game.toml` is read with today's gth-config, so a change to the game.toml
/// schema that an old file no longer parses under is a format change too: bump this and
/// read old recordings through a migration (docs/recording-format.md, "Versioning").
pub const FORMAT_VERSION: u32 = 1;

/// The parsed `meta.toml` (docs/recording-format.md, "meta.toml").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub format_version: u32,
    /// The game's directory name under `games/`.
    pub game_id: String,
    /// SHA-1 of the recording's `game.toml` (the config it was made with, in canonical form).
    pub game_config_sha1: String,
    pub recorder: RecorderKind,
    /// Who played: a person's handle, or a policy and its checkpoint.
    pub player: String,
    /// The license the recording is shared under, e.g. `CC-BY-NC-4.0`.
    pub license: String,
    /// Seconds since the Unix epoch when recording started.
    pub created_unix: u64,
    /// The core's frame rate (`retro_get_system_av_info`).
    pub fps: f64,
    /// Emulated frames in `inputs.bin`.
    pub frames: u64,
    /// Agent steps: the frames with the step-end flag.
    pub steps: u64,
    /// Frames between two checks in `checks.bin` (a last check follows the last frame).
    pub checks_every: u32,
    pub core: CoreMeta,
    /// The content file the core loaded; `None` when it loaded none, or a path that doesn't
    /// exist (a clone `in_parent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentMeta>,
    pub start: StartMeta,
    /// The harness build: `_native.build_info()` (version, git commit, target, rustc).
    pub harness: BTreeMap<String, String>,
    /// A side recording branched off another (a death clip from gth-play's rewind): where
    /// it starts in its parent and why. `None` for a recording made from its own start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<BranchMeta>,
    /// The rewinds spliced into this recording, in the order they happened: each truncated
    /// the recording to its `frame` and play went on from there. Only rewinds still on the
    /// recording's timeline are kept (a later rewind to an earlier frame drops them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rewind: Vec<RewindMeta>,
}

/// Why gth-play rewound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewindCause {
    /// The game's `lives` RAM variable dropped: a death, rewound automatically.
    Lives,
    /// The player pressed the rewind hotkey.
    Manual,
}

impl RewindCause {
    /// The name in `meta.toml`.
    pub fn name(self) -> &'static str {
        match self {
            RewindCause::Lives => "lives",
            RewindCause::Manual => "manual",
        }
    }
}

/// `[branch]`: a side recording's place in the recording it branched from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchMeta {
    /// The parent recording's directory name (`<id>.gthrec`), beside this one.
    pub parent: String,
    /// The parent's frames before this recording's start state: its start state is the
    /// parent's state after `frame` frames, on the parent's timeline when the branch was made
    /// (a later rewind of the parent to an earlier frame replaces that timeline).
    pub frame: u64,
    pub cause: RewindCause,
}

/// One `[[rewind]]`: the recording was truncated to `frame` frames and went on from there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewindMeta {
    /// The frames kept: the frames after it were replaced.
    pub frame: u64,
    pub cause: RewindCause,
    /// The side recording holding the replaced frames (`<id>.gthrec`, beside this one), whose
    /// `[branch]` points back here.
    pub clip: String,
}

/// Who or what produced the inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecorderKind {
    /// A person playing (`gth-play`).
    Human,
    /// A learned policy.
    Policy,
    /// Scripted or random inputs (tests, harness rollouts).
    Scripted,
    /// Converted from another format (a MAME `.inp`).
    Converted,
}

impl RecorderKind {
    pub const ALL: [RecorderKind; 4] = [
        RecorderKind::Human,
        RecorderKind::Policy,
        RecorderKind::Scripted,
        RecorderKind::Converted,
    ];

    /// The name in `meta.toml`.
    pub fn name(self) -> &'static str {
        match self {
            RecorderKind::Human => "human",
            RecorderKind::Policy => "policy",
            RecorderKind::Scripted => "scripted",
            RecorderKind::Converted => "converted",
        }
    }

    /// The kind named `name` in `meta.toml`, or `None`.
    pub fn from_name(name: &str) -> Option<RecorderKind> {
        RecorderKind::ALL
            .into_iter()
            .find(|kind| kind.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreMeta {
    /// The core's name in game.toml and `cores.lock`.
    pub name: String,
    /// What `retro_get_system_info` reported.
    pub library_name: String,
    pub library_version: String,
    /// The core's `cores.lock` entry (a `gth_config::CoreLock`), when the caller has one. Kept
    /// as a plain table, so a later change to the lock's schema doesn't make old recordings
    /// unreadable: nothing reads it back to replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<toml::Table>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentMeta {
    /// The file name the core was given.
    pub file: String,
    pub sha1: String,
}

/// The savestate the recording starts from, `start.state` (zstd).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartMeta {
    /// SHA-1 of the uncompressed state.
    pub sha1: String,
    /// Uncompressed size in bytes.
    pub size: u64,
}

#[derive(Deserialize)]
struct VersionOnly {
    format_version: u32,
}

impl Meta {
    /// Parses `meta.toml`, checking the format version first, so a recording from another
    /// version is refused by its version rather than by a field this build doesn't know.
    pub fn parse(text: &str, path: &Path) -> Result<Meta> {
        let error = |e: toml::de::Error| format_error(path, e.to_string());
        // `VersionOnly` ignores the other fields.
        let VersionOnly { format_version } = toml::from_str(text).map_err(error)?;
        if format_version != FORMAT_VERSION {
            return Err(format_error(
                path,
                format!(
                    "format version {format_version}; this build reads version {FORMAT_VERSION}"
                ),
            ));
        }
        toml::from_str(text).map_err(error)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("every field serializes to TOML")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_names_are_the_meta_toml_names() {
        #[derive(Serialize)]
        struct One {
            recorder: RecorderKind,
        }
        for kind in RecorderKind::ALL {
            let text = toml::to_string(&One { recorder: kind }).unwrap();
            assert_eq!(text.trim(), format!("recorder = \"{}\"", kind.name()));
            assert_eq!(RecorderKind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(RecorderKind::from_name("Human"), None);
    }

    #[test]
    fn branch_and_rewinds_round_trip_and_are_optional() {
        let mut meta = Meta {
            format_version: FORMAT_VERSION,
            game_id: "gunhed".into(),
            game_config_sha1: "0".repeat(40),
            recorder: RecorderKind::Human,
            player: "test".into(),
            license: "CC0-1.0".into(),
            created_unix: 0,
            fps: 60.0,
            frames: 10,
            steps: 10,
            checks_every: 60,
            core: CoreMeta {
                name: "gth_testcore".into(),
                library_name: "gth-testcore".into(),
                library_version: "0".into(),
                lock: None,
            },
            content: None,
            start: StartMeta {
                sha1: "0".repeat(40),
                size: 1,
            },
            harness: BTreeMap::new(),
            branch: None,
            rewind: Vec::new(),
        };
        let plain = meta.to_toml();
        assert!(
            !plain.contains("branch") && !plain.contains("rewind"),
            "{plain}"
        );
        assert_eq!(Meta::parse(&plain, Path::new("m")).unwrap(), meta);

        meta.branch = Some(BranchMeta {
            parent: "gunhed-main.gthrec".into(),
            frame: 120,
            cause: RewindCause::Lives,
        });
        meta.rewind = vec![
            RewindMeta {
                frame: 5,
                cause: RewindCause::Manual,
                clip: "a.gthrec".into(),
            },
            RewindMeta {
                frame: 7,
                cause: RewindCause::Lives,
                clip: "b.gthrec".into(),
            },
        ];
        let text = meta.to_toml();
        assert!(
            text.contains("[branch]") && text.contains("[[rewind]]"),
            "{text}"
        );
        assert!(text.contains("cause = \"lives\""), "{text}");
        assert_eq!(Meta::parse(&text, Path::new("m")).unwrap(), meta);
    }
}
