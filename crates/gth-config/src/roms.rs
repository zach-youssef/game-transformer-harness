//! Finding the user's files: the ROM directory (from `GTH_ROM_DIR` or the user config file)
//! and files in it by SHA-1 (docs/roms.md, "Finding files").

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha1::{Digest, Sha1};

use crate::{Error, FileSpec, GameConfig, Hashes, Result, StateFile, invalid};

/// The per-user config file, `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserConfig {
    /// The ROM directory; a relative path is relative to the config file's directory.
    rom_dir: Option<PathBuf>,
}

/// The user config file: `GTH_CONFIG` if set, else `%APPDATA%\gtharness\config.toml` on
/// Windows and `$XDG_CONFIG_HOME/gtharness/config.toml` (default `~/.config`) elsewhere.
/// `None` if none of those variables is set.
pub fn user_config_path() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|value: &OsString| !value.is_empty());
    if let Some(path) = var("GTH_CONFIG") {
        return Some(path.into());
    }
    let base = if cfg!(windows) {
        var("APPDATA").map(PathBuf::from)
    } else {
        var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| var("HOME").map(|home| Path::new(&home).join(".config")))
    }?;
    Some(base.join("gtharness").join("config.toml"))
}

/// The ROM directory: `GTH_ROM_DIR` if set, else `rom_dir` in the user config file
/// ([`user_config_path`]), else `None`. Fails if the config file exists but is invalid.
pub fn rom_dir() -> Result<Option<PathBuf>> {
    let env = std::env::var_os("GTH_ROM_DIR").filter(|value| !value.is_empty());
    rom_dir_from(env, user_config_path().as_deref())
}

fn rom_dir_from(env: Option<OsString>, config: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(dir) = env {
        return Ok(Some(dir.into()));
    }
    let Some(config) = config.filter(|path| path.is_file()) else {
        return Ok(None);
    };
    let what = config.display().to_string();
    let user: UserConfig =
        toml::from_str(&crate::read(config)?).map_err(|error| invalid(&what, error.to_string()))?;
    let base = config.parent().unwrap_or(Path::new(""));
    Ok(user.rom_dir.map(|dir| base.join(dir)))
}

fn io_error(path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.display().to_string(),
        source,
    }
}

pub(crate) fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The SHA-1 of a file, as 40 lowercase hex digits.
pub fn sha1_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(|error| io_error(path, error))?;
    let mut hasher = Sha1::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| io_error(path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn matches(path: &Path, hashes: &Hashes) -> Result<bool> {
    let size = std::fs::metadata(path)
        .map_err(|error| io_error(path, error))?
        .len();
    Ok(size == hashes.size && sha1_file(path)? == hashes.sha1)
}

/// Whether `path` is the file `spec` describes: same size and SHA-1, or for an archive, same
/// file name (its members are checked by [`GameConfig::audit`](crate::GameConfig::audit)).
/// `false` if it doesn't exist.
pub fn check_file(path: &Path, spec: &FileSpec) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    match &spec.hashes {
        Some(hashes) => matches(path, hashes),
        None => Ok(path.file_name() == Path::new(&spec.file).file_name()),
    }
}

/// Finds the file `spec` describes under `dir`, searching subdirectories too: by size and
/// SHA-1, whatever its name (files of the right size are hashed, the expected name first),
/// or, for an archive, by name. `None` if it isn't there. A subdirectory or file that can't be
/// read is skipped; if the file isn't found elsewhere, that I/O error is returned, since the
/// file may be what couldn't be read.
pub fn find_file(dir: &Path, spec: &FileSpec) -> Result<Option<PathBuf>> {
    if !dir.is_dir() {
        return Err(invalid(
            dir.display().to_string(),
            "the ROM directory doesn't exist",
        ));
    }
    let named = dir.join(&spec.file);
    let mut errors = Vec::new();
    match check_file(&named, spec) {
        Ok(true) => return Ok(Some(named)),
        Ok(false) => {}
        Err(error) => errors.push(error),
    }
    let mut candidates = Vec::new();
    walk(dir, &mut candidates, &mut errors);
    candidates.sort();
    for path in candidates.into_iter().filter(|path| *path != named) {
        match check_file(&path, spec) {
            Ok(true) => return Ok(Some(path)),
            Ok(false) => {}
            Err(error) => errors.push(error),
        }
    }
    match errors.into_iter().next() {
        Some(error) => Err(error),
        None => Ok(None),
    }
}

/// The specs in `specs` that aren't at `dir/<file>` ([`check_file`]).
fn missing<'a>(dir: &Path, specs: &'a [FileSpec]) -> Result<Vec<&'a FileSpec>> {
    let mut missing = Vec::new();
    for spec in specs {
        if !check_file(&dir.join(&spec.file), spec)? {
            missing.push(spec);
        }
    }
    Ok(missing)
}

impl GameConfig {
    /// The content path to give the core, found under `rom_dir` with [`find_file`]: the
    /// content file, or for content `in_parent` (a clone in a merged set),
    /// `<directory of the first parent>/<content file>`, a path that needn't exist. `None` if
    /// the content (or that parent) isn't there.
    pub fn find_content(&self, rom_dir: &Path) -> Result<Option<PathBuf>> {
        if !self.content.in_parent {
            return find_file(rom_dir, &self.content);
        }
        let parent = self.content.parents.first().ok_or_else(|| {
            invalid(
                &self.content.file,
                "in_parent needs `parents`, the first being the archive that holds it",
            )
        })?;
        Ok(find_file(rom_dir, parent)?.map(|found| found.with_file_name(&self.content.file)))
    }

    /// The `bios` files that aren't at their paths under `system_dir` ([`check_file`]).
    pub fn missing_bios(&self, system_dir: &Path) -> Result<Vec<&FileSpec>> {
        missing(system_dir, &self.bios)
    }

    /// The content's `parents` that aren't next to `content` (the found content file), where
    /// the core looks for them.
    pub fn missing_parents(&self, content: &Path) -> Result<Vec<&FileSpec>> {
        missing(
            content.parent().unwrap_or(Path::new("")),
            &self.content.parents,
        )
    }
}

impl StateFile {
    /// Checks that `state` (the savestate file's bytes) has the config's SHA-1.
    pub fn check(&self, state: &[u8]) -> Result<()> {
        let sha1 = hex(&Sha1::digest(state));
        if sha1 == self.sha1 {
            Ok(())
        } else {
            Err(invalid(
                &self.file,
                format!(
                    "its SHA-1 is {sha1}, not {} as game.toml says: record the reset state \
                     again with this core build, or fix the hash",
                    self.sha1
                ),
            ))
        }
    }
}

/// Every file under `dir`. Symlinked directories aren't followed, so a loop can't trap it.
fn walk(dir: &Path, files: &mut Vec<PathBuf>, errors: &mut Vec<Error>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => return errors.push(io_error(dir, error)),
    };
    for entry in entries {
        let kind = entry.and_then(|entry| Ok((entry.file_type()?, entry.path())));
        match kind {
            Ok((kind, path)) if kind.is_dir() => walk(&path, files, errors),
            Ok((_, path)) if path.is_file() => files.push(path),
            Ok(_) => {}
            Err(error) => errors.push(io_error(dir, error)),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh directory under the system temp dir, removed on drop.
    pub(crate) struct TempDir(pub(crate) PathBuf);

    impl TempDir {
        pub(crate) fn new(name: &str) -> TempDir {
            let path =
                std::env::temp_dir().join(format!("gth-config-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // SHA-1 of b"abc", the FIPS 180 test vector.
    const ABC: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

    fn spec(file: &str, sha1: &str) -> FileSpec {
        FileSpec {
            file: file.into(),
            hashes: Some(Hashes {
                size: 3,
                sha1: sha1.into(),
                crc32: "352441c2".into(),
            }),
            members: Vec::new(),
            parents: Vec::new(),
            in_parent: false,
        }
    }

    #[test]
    fn finds_files_by_sha1_whatever_their_name() {
        let dir = TempDir::new("find");
        std::fs::create_dir_all(dir.0.join("sub/deeper")).unwrap();
        std::fs::write(dir.0.join("other.bin"), b"abd").unwrap();
        std::fs::write(dir.0.join("sub/deeper/renamed.bin"), b"abc").unwrap();
        assert_eq!(
            sha1_file(&dir.0.join("sub/deeper/renamed.bin")).unwrap(),
            ABC
        );
        let found = find_file(&dir.0, &spec("game.bin", ABC)).unwrap();
        assert_eq!(found, Some(dir.0.join("sub/deeper/renamed.bin")));
        std::fs::write(dir.0.join("game.bin"), b"abc").unwrap();
        let found = find_file(&dir.0, &spec("game.bin", ABC)).unwrap();
        assert_eq!(
            found,
            Some(dir.0.join("game.bin")),
            "the expected name first"
        );
        let missing = spec("game.bin", &"0".repeat(40));
        assert_eq!(find_file(&dir.0, &missing).unwrap(), None);
        assert!(!check_file(&dir.0.join("other.bin"), &spec("other.bin", ABC)).unwrap());
        assert!(find_file(&dir.0.join("nope"), &missing).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_files_and_directories_are_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("find-unreadable");
        std::fs::create_dir_all(dir.0.join("a-locked")).unwrap();
        std::fs::create_dir_all(dir.0.join("z")).unwrap();
        std::fs::write(dir.0.join("a-locked.bin"), b"abd").unwrap();
        std::fs::write(dir.0.join("z/renamed.bin"), b"abc").unwrap();
        let mode = |path: &str, mode| {
            std::fs::set_permissions(dir.0.join(path), std::fs::Permissions::from_mode(mode))
                .unwrap()
        };
        mode("a-locked", 0o000);
        mode("a-locked.bin", 0o000);
        // Root reads them anyway.
        let locked = std::fs::File::open(dir.0.join("a-locked.bin")).is_err();
        let found = find_file(&dir.0, &spec("game.bin", ABC));
        let missing = find_file(&dir.0, &spec("game.bin", &"0".repeat(40)));
        mode("a-locked", 0o755);
        mode("a-locked.bin", 0o644);
        if !locked {
            return;
        }
        assert_eq!(
            found.unwrap(),
            Some(dir.0.join("z/renamed.bin")),
            "found past them"
        );
        let error = missing.unwrap_err().to_string();
        assert!(
            error.contains("a-locked"),
            "not found, so the error: {error}"
        );
    }

    #[test]
    fn bios_files_are_checked_by_name_and_hash() {
        let dir = TempDir::new("bios");
        let mut config = crate::GameConfig::parse(
            r#"
core = "c"
orientation = 0
content = { file = "c.bin", size = 3, sha1 = "a9993e364706816aba3e25717850c26c9cd0d89d", crc32 = "352441c2" }
buttons = { a = "shot" }
reset = { state = { file = "s.state", sha1 = "a9993e364706816aba3e25717850c26c9cd0d89d" } }
episode = { terminate = "1" }
"#,
            "test",
        )
        .unwrap();
        config.bios = vec![
            spec("bios.bin", ABC),
            spec("other.bin", ABC),
            spec("fbneo/sub.bin", ABC),
        ];
        std::fs::create_dir_all(dir.0.join("fbneo")).unwrap();
        std::fs::write(dir.0.join("bios.bin"), b"abc").unwrap();
        std::fs::write(dir.0.join("other.bin"), b"abd").unwrap();
        std::fs::write(dir.0.join("sub.bin"), b"abc").unwrap();
        let missing = config.missing_bios(&dir.0).unwrap();
        assert_eq!(
            missing,
            [&config.bios[1], &config.bios[2]],
            "a wrong hash; a file outside its subdirectory"
        );
        std::fs::write(dir.0.join("fbneo/sub.bin"), b"abc").unwrap();
        assert_eq!(config.missing_bios(&dir.0).unwrap(), [&config.bios[1]]);
        assert_eq!(config.missing_bios(&dir.0.join("nope")).unwrap().len(), 3);
    }

    #[test]
    fn parents_are_checked_next_to_the_content() {
        let dir = TempDir::new("parents");
        std::fs::create_dir_all(dir.0.join("arcade")).unwrap();
        let mut config = crate::GameConfig::parse(&crate::game::tests::full(), "full").unwrap();
        let content = dir.0.join("arcade/clone.zip");
        let parent = &config.content.parents[0];
        assert_eq!(config.missing_parents(&content).unwrap(), [parent]);
        std::fs::write(dir.0.join("parent.zip"), b"zip").unwrap();
        assert_eq!(
            config.missing_parents(&content).unwrap().len(),
            1,
            "not next to the content"
        );
        std::fs::write(dir.0.join("arcade/parent.zip"), b"zip").unwrap();
        assert!(config.missing_parents(&content).unwrap().is_empty());
        config.content.parents.push(spec("hashed.bin", ABC));
        std::fs::write(dir.0.join("arcade/hashed.bin"), b"abd").unwrap();
        let missing = config.missing_parents(&content).unwrap();
        assert_eq!(missing, [&config.content.parents[1]], "wrong hash");
    }

    #[test]
    fn a_clone_in_its_parents_archive_is_found_through_the_parent() {
        let dir = TempDir::new("merged");
        std::fs::create_dir_all(dir.0.join("arcade")).unwrap();
        let config = crate::GameConfig::parse(&crate::game::tests::merged(), "merged").unwrap();
        assert_eq!(config.find_content(&dir.0).unwrap(), None, "no parent");
        std::fs::write(dir.0.join("arcade/parent.zip"), b"zip").unwrap();
        let content = config.find_content(&dir.0).unwrap().unwrap();
        assert_eq!(content, dir.0.join("arcade/clone.zip"));
        assert!(!content.exists(), "the clone has no file of its own");
        assert!(config.missing_parents(&content).unwrap().is_empty());
        let full = crate::GameConfig::parse(&crate::game::tests::full(), "full").unwrap();
        assert_eq!(
            full.find_content(&dir.0).unwrap(),
            None,
            "not by the parent"
        );
    }

    #[test]
    fn reset_states_are_checked_by_sha1() {
        let state = StateFile {
            file: "s.state".into(),
            sha1: ABC.into(),
        };
        state.check(b"abc").unwrap();
        let error = state.check(b"abd").unwrap_err().to_string();
        assert!(error.contains("s.state") && error.contains(ABC), "{error}");
    }

    #[test]
    fn finds_archives_by_name() {
        let dir = TempDir::new("archive");
        std::fs::create_dir_all(dir.0.join("arcade")).unwrap();
        std::fs::write(dir.0.join("arcade/set.zip"), b"zip").unwrap();
        let archive = FileSpec {
            file: "set.zip".into(),
            hashes: None,
            members: Vec::new(),
            parents: Vec::new(),
            in_parent: false,
        };
        let found = find_file(&dir.0, &archive).unwrap();
        assert_eq!(found, Some(dir.0.join("arcade/set.zip")));
    }

    #[test]
    fn rom_dir_comes_from_the_env_then_the_config_file() {
        let dir = TempDir::new("user");
        let config = dir.0.join("config.toml");
        assert_eq!(rom_dir_from(None, Some(&config)).unwrap(), None, "no file");
        assert_eq!(rom_dir_from(None, None).unwrap(), None);
        std::fs::write(&config, "rom_dir = \"roms\"\n").unwrap();
        assert_eq!(
            rom_dir_from(None, Some(&config)).unwrap(),
            Some(dir.0.join("roms"))
        );
        let env = Some(OsString::from("/elsewhere"));
        assert_eq!(
            rom_dir_from(env, Some(&config)).unwrap(),
            Some("/elsewhere".into())
        );
        std::fs::write(&config, "rom_dri = \"roms\"\n").unwrap();
        let error = rom_dir_from(None, Some(&config)).unwrap_err().to_string();
        assert!(error.contains("unknown field"), "{error}");
        std::fs::write(&config, "").unwrap();
        assert_eq!(
            rom_dir_from(None, Some(&config)).unwrap(),
            None,
            "no rom_dir"
        );
    }
}
