//! `cores.lock`: the pinned build of each libretro core (filled by `cores fetch`, P1).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Result, invalid};

/// The whole lock file: one `[cores.<name>]` table per core.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoresLock {
    #[serde(default)]
    pub cores: BTreeMap<String, CoreLock>,
}

/// One pinned core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreLock {
    /// The upstream git repository, e.g. `https://github.com/libretro/FBNeo`.
    pub repo: String,
    /// The full commit hash built.
    pub commit: String,
    /// What `retro_get_system_info` reports as `library_version` for this build.
    pub library_version: String,
    /// SHA-256 of the built library per target triple (e.g. `aarch64-apple-darwin`).
    pub artifacts: BTreeMap<String, String>,
}

impl CoresLock {
    pub fn parse(text: &str, what: &str) -> Result<CoresLock> {
        let lock: CoresLock =
            toml::from_str(text).map_err(|error| invalid(what, error.to_string()))?;
        for (name, core) in &lock.cores {
            let is_hex = |s: &str, n: usize| {
                s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            };
            if !is_hex(&core.commit, 40) {
                return Err(invalid(what, format!("{name}: commit isn't a full hash")));
            }
            if let Some((target, _)) = core.artifacts.iter().find(|(_, h)| !is_hex(h, 64)) {
                return Err(invalid(
                    what,
                    format!("{name}: artifact for {target} isn't a SHA-256"),
                ));
            }
        }
        Ok(lock)
    }

    pub fn load(path: &Path) -> Result<CoresLock> {
        Self::parse(&crate::read(path)?, &path.display().to_string())
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("every field serializes to TOML")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"
[cores.fbneo]
repo = "https://github.com/libretro/FBNeo"
commit = "0123456789abcdef0123456789abcdef01234567"
library_version = "v1.0.0.03 0123456"

[cores.fbneo.artifacts]
aarch64-apple-darwin = "0000000000000000000000000000000000000000000000000000000000000000"
x86_64-pc-windows-msvc = "1111111111111111111111111111111111111111111111111111111111111111"
"#;

    #[test]
    fn round_trips() {
        let lock = CoresLock::parse(LOCK, "cores.lock").unwrap();
        assert_eq!(lock.cores["fbneo"].artifacts.len(), 2);
        assert_eq!(CoresLock::parse(&lock.to_toml(), "again").unwrap(), lock);
        assert_eq!(CoresLock::parse("", "empty").unwrap(), CoresLock::default());
    }

    #[test]
    fn rejects_short_hashes_and_unknown_fields() {
        for (from, to, reason) in [
            ("commit = \"0123", "commit = \"123", "full hash"),
            (
                "= \"1111",
                "= \"111",
                "x86_64-pc-windows-msvc isn't a SHA-256",
            ),
            ("repo =", "branch = \"main\"\nrepo =", "unknown field"),
        ] {
            let error = CoresLock::parse(&LOCK.replacen(from, to, 1), "cores.lock")
                .unwrap_err()
                .to_string();
            assert!(error.contains(reason), "{error}");
        }
    }
}
