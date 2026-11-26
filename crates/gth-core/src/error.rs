//! The error type of every fallible `gth-core` operation.

use std::path::PathBuf;

/// Everything that can go wrong in the host. Not `#[non_exhaustive]`: `gth-py` matches every
/// variant, so adding one fails to compile until it is given a Python exception.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A libretro core couldn't be loaded or initialized.
    #[error("cannot load core {path}: {reason}")]
    CoreLoad { path: PathBuf, reason: String },
    /// The content file couldn't be read, for a core that loads content from memory.
    #[error("cannot read content {path} for core {core}: {reason}")]
    Content {
        core: PathBuf,
        path: PathBuf,
        reason: String,
    },
    /// A core couldn't save or load a savestate.
    #[error("savestate error in core {core}: {reason}")]
    Savestate { core: String, reason: String },
    /// A RAM variable couldn't be read or decoded.
    #[error("cannot read RAM variable {name}: {reason}")]
    Ram { name: String, reason: String },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_cause() {
        let load = Error::CoreLoad {
            path: "cores/x.so".into(),
            reason: "missing symbol retro_run".into(),
        };
        assert_eq!(
            load.to_string(),
            "cannot load core cores/x.so: missing symbol retro_run"
        );
    }
}
