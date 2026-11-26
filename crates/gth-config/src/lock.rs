//! `cores.lock`: the pinned build of each libretro core (docs/cores.md): the upstream commit
//! the cores workflow builds, the version it reports and the SHA-256 of each platform's build,
//! which `gth cores fetch` checks.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::roms::{hex, sha256_file};
use crate::{Result, invalid};

/// The comment `to_toml` puts first, since serializing drops the file's own comments.
const HEADER: &str = "# Pinned libretro core builds. docs/cores.md says how to bump a pin.\n\n";

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

    /// The lock as TOML, after a fixed header comment.
    pub fn to_toml(&self) -> String {
        HEADER.to_owned() + &toml::to_string(self).expect("every field serializes to TOML")
    }

    /// The core called `name`; an error naming the cores there are if there's none.
    pub fn core(&self, name: &str) -> Result<&CoreLock> {
        self.cores.get(name).ok_or_else(|| {
            let names: Vec<&str> = self.cores.keys().map(String::as_str).collect();
            invalid(
                "cores.lock",
                format!("no core {name} (it has {})", names.join(", ")),
            )
        })
    }

    /// The tag of the GitHub release with this lock's builds: `cores-` and 16 hex digits of a
    /// SHA-256 over each core's name, repository and commit. The versions and artifact hashes
    /// are left out, so writing them into the lock after the release keeps its tag.
    pub fn release_tag(&self) -> String {
        let mut hasher = Sha256::new();
        for (name, core) in &self.cores {
            hasher.update(format!("{name}\n{}\n{}\n", core.repo, core.commit));
        }
        format!("cores-{}", &hex(&hasher.finalize())[..16])
    }

    /// This lock with each core's `artifacts` for `targets` set to the SHA-256 of
    /// `<dir>/<target>/<library file>` ([`library_file`]), as the cores workflow lays out its
    /// builds; other targets keep theirs. Fails if a build is missing.
    pub fn with_builds(&self, dir: &Path, targets: &[&str]) -> Result<CoresLock> {
        let mut lock = self.clone();
        for (name, core) in &mut lock.cores {
            for target in targets {
                let path = dir.join(target).join(library_file(name, target));
                core.artifacts
                    .insert((*target).to_owned(), sha256_file(&path)?);
            }
        }
        Ok(lock)
    }
}

impl CoreLock {
    /// Fails unless `path` is the build of core `name` for `target` that the lock pins.
    pub fn check_artifact(&self, name: &str, target: &str, path: &Path) -> Result<()> {
        let pinned = self.artifacts.get(target).ok_or_else(|| {
            invalid(
                "cores.lock",
                format!(
                    "{name} has no build for {target}: the cores release for this lock may not \
                     have been written into it yet (docs/cores.md)"
                ),
            )
        })?;
        let found = sha256_file(path)?;
        if &found != pinned {
            return Err(invalid(
                path.display().to_string(),
                format!("SHA-256 {found}, but cores.lock pins {pinned} for {name} on {target}"),
            ));
        }
        Ok(())
    }

    /// Fails unless `reported` (`retro_get_system_info`'s `library_version`) is the pinned
    /// version, which tells a core built from another commit or recipe from this one.
    pub fn check_library_version(&self, name: &str, reported: &str) -> Result<()> {
        if reported != self.library_version {
            return Err(invalid(
                name,
                format!(
                    "the core reports library_version {reported:?}, but cores.lock pins {:?}",
                    self.library_version
                ),
            ));
        }
        Ok(())
    }
}

/// The library file name of core `name` for a target triple, as its Makefile names it and
/// `RetroEnv.from_game` looks for it: `<name>_libretro.<dll|dylib|so>`.
pub fn library_file(name: &str, target: &str) -> String {
    format!("{name}_libretro.{}", library_extension(target))
}

/// The release asset name of core `name`'s build for a target triple:
/// `<name>_libretro.<target>.<dll|dylib|so>`.
pub fn asset_name(name: &str, target: &str) -> String {
    format!("{name}_libretro.{target}.{}", library_extension(target))
}

fn library_extension(target: &str) -> &'static str {
    if target.contains("-windows") {
        "dll"
    } else if target.contains("-apple-") {
        "dylib"
    } else {
        "so"
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
    fn names_files_per_target() {
        assert_eq!(
            library_file("fbneo", "x86_64-pc-windows-msvc"),
            "fbneo_libretro.dll"
        );
        assert_eq!(
            library_file("fceumm", "aarch64-apple-darwin"),
            "fceumm_libretro.dylib"
        );
        assert_eq!(
            asset_name("fceumm", "x86_64-unknown-linux-gnu"),
            "fceumm_libretro.x86_64-unknown-linux-gnu.so"
        );
    }

    #[test]
    fn release_tag_covers_the_sources_only() {
        let lock = CoresLock::parse(LOCK, "cores.lock").unwrap();
        let tag = lock.release_tag();
        assert!(tag.starts_with("cores-") && tag.len() == 22, "{tag}");
        let mut filled = lock.clone();
        let fbneo = filled.cores.get_mut("fbneo").unwrap();
        fbneo.library_version = "other".into();
        fbneo.artifacts.clear();
        assert_eq!(
            filled.release_tag(),
            tag,
            "versions and hashes don't change it"
        );
        let bumped = CoresLock::parse(&LOCK.replace("01234567\"", "89abcdef\""), "x").unwrap();
        assert_ne!(bumped.release_tag(), tag, "a new commit does");
        let renamed = CoresLock::parse(&LOCK.replace("cores.fbneo", "cores.fbn"), "x").unwrap();
        assert_ne!(renamed.release_tag(), tag);
    }

    #[test]
    fn checks_artifacts_and_versions() {
        let dir = std::env::temp_dir().join(format!("gth-lock-{}", std::process::id()));
        let target = "aarch64-apple-darwin";
        std::fs::create_dir_all(dir.join(target)).unwrap();
        let build = dir.join(target).join("fbneo_libretro.dylib");
        std::fs::write(&build, b"a core").unwrap();
        let lock = CoresLock::parse(LOCK, "cores.lock").unwrap();

        let filled = lock.with_builds(&dir, &[target]).unwrap();
        let fbneo = filled.core("fbneo").unwrap();
        fbneo.check_artifact("fbneo", target, &build).unwrap();
        assert_eq!(
            fbneo.artifacts["x86_64-pc-windows-msvc"],
            lock.cores["fbneo"].artifacts["x86_64-pc-windows-msvc"],
            "other targets keep their hashes"
        );
        let error = lock.cores["fbneo"]
            .check_artifact("fbneo", target, &build)
            .unwrap_err()
            .to_string();
        assert!(error.contains("but cores.lock pins 0000"), "{error}");
        let error = (fbneo.check_artifact("fbneo", "riscv64gc-unknown-linux-gnu", &build))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no build for riscv64gc"), "{error}");
        let missing = lock.with_builds(&dir, &["x86_64-unknown-linux-gnu"]);
        assert!(
            missing
                .unwrap_err()
                .to_string()
                .contains("fbneo_libretro.so")
        );

        fbneo
            .check_library_version("fbneo", "v1.0.0.03 0123456")
            .unwrap();
        let error = (fbneo.check_library_version("fbneo", "v1.0.0.03 89abcde"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("pins \"v1.0.0.03 0123456\""), "{error}");
        let error = lock.core("fceumm").unwrap_err().to_string();
        assert!(error.contains("no core fceumm (it has fbneo)"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
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
