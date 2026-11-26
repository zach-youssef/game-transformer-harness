//! Game and core config types (serde, TOML): `game.toml` (docs/games.md), `cores.lock`, RAM
//! variables, the episode condition and the action space (docs/actions.md). Parsing validates
//! everything it can without a core, so a config that loads is one the harness can use. `roms`
//! finds the user's ROM directory and files in it, and `audit` checks them by hash (docs/roms.md).

mod actions;
mod audit;
mod condition;
mod game;
mod lock;
mod ram;
mod roms;

pub use actions::{Action, ActionSpace, Direction, Gamepad};
pub use audit::{Entry, FileAudit, Found, GameAudit, MemberAudit, Role, Status};
pub use condition::Condition;
pub use game::{
    Crop, Episode, FileSpec, GameConfig, Hashes, Member, Orientation, Reset, ResetStep,
    RetroButton, Slot, StateFile,
};
pub use lock::{CoreLock, CoresLock};
pub use ram::{Encoding, Location, MemoryId, RamVar};
pub use roms::{check_file, find_file, rom_dir, sha1_file, user_config_path};

/// Everything that can go wrong reading a config. Not `#[non_exhaustive]`: `gth-py` maps every
/// variant to a Python exception.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file couldn't be read.
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    /// A config doesn't parse or breaks a rule; `what` names the file or value.
    #[error("invalid {what}: {reason}")]
    Invalid { what: String, reason: String },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn invalid(what: impl Into<String>, reason: impl Into<String>) -> Error {
    Error::Invalid {
        what: what.into(),
        reason: reason.into(),
    }
}

/// Reads `path` as text, for the `load` functions.
fn read(path: &std::path::Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })
}
