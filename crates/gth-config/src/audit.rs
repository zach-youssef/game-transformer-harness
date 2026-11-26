//! The ROM audit (docs/roms.md, "Auditing"): checks a game's content, parents and BIOS files,
//! and every member of their archives, against `game.toml`'s sizes and hashes.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Serialize, Serializer};
use sha1::{Digest, Sha1};

use crate::roms::{find_file, hex};
use crate::{FileSpec, GameConfig, Hashes, Member, Result, invalid};

/// Where a file comes from in `game.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Content,
    Parent,
    Bios,
}

/// The verdict on a file or an archive member, ordered from best to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Present with the expected size and hashes (an archive: every member is).
    Ok,
    /// Not there.
    Missing,
    /// Present, but its size or a hash differs (an archive: a member is missing or bad).
    BadHash,
    /// Present but unreadable: an I/O error, an archive that doesn't parse, or a member that
    /// fails its own CRC or uses a compression method other than store and deflate.
    Unreadable,
}

/// The size and hashes found for a file or archive member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Found {
    pub size: u64,
    pub crc32: String,
    pub sha1: String,
}

impl Found {
    fn matches(&self, size: u64, crc32: &str, sha1: &str) -> bool {
        self.size == size && self.crc32 == crc32 && self.sha1 == sha1
    }
}

/// An archive member, as found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// Its path inside the archive (MAME's merged sets keep clone files in `<clone>/`).
    pub name: String,
    /// Its size and hashes, if it could be read.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub found: Option<Found>,
    /// Why it couldn't be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One expected archive member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemberAudit {
    pub name: String,
    pub status: Status,
    /// The archive member it was matched to, when that has another name (members are
    /// matched by their hashes first: FBNeo finds them by CRC, whatever their name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_as: Option<String>,
    /// What the archive holds under its name, when it differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found: Option<Found>,
    /// Why it couldn't be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One file of a game: the content, a parent or a BIOS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileAudit {
    pub role: Role,
    /// The name in `game.toml`.
    pub file: String,
    pub status: Status,
    /// Where it was found (or, if missing, where it was looked for when that is one place).
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "lossy")]
    pub path: Option<PathBuf>,
    /// A plain file's size and hashes, when they differ from `game.toml`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found: Option<Found>,
    /// For an archive: each expected member.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberAudit>,
    /// For an archive: members that match no expected member. Not an error: a merged set
    /// holds its clones.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<Entry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A whole game's audit, from [`GameConfig::audit`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameAudit {
    /// The worst of its files' statuses: `Ok` when every file is, `Missing` when some are
    /// missing and none is bad or unreadable.
    pub status: Status,
    /// The content first, then its parents, then the BIOS files.
    pub files: Vec<FileAudit>,
}

impl GameAudit {
    /// The audit as JSON (docs/roms.md, "Auditing").
    pub fn to_json(&self) -> String {
        // Only strings (paths lossily), numbers and enums: serializing can't fail.
        serde_json::to_string(self).expect("the audit serializes to JSON")
    }
}

/// Serializes a path as a string, replacing what isn't UTF-8 (JSON strings must be).
fn lossy<S: Serializer>(
    path: &Option<PathBuf>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    match path {
        Some(path) => serializer.serialize_some(&path.to_string_lossy()),
        None => serializer.serialize_none(),
    }
}

impl GameConfig {
    /// Audits the game's files: the content found under `rom_dir` ([`find_file`]), its parents
    /// beside it and the BIOS files at their paths under `system_dir`, each archive member by
    /// size, CRC-32 and SHA-1. Content `in_parent` is audited inside its first parent's
    /// archive. Content with the wrong hashes is found by name at the top of `rom_dir` only.
    /// Fails only if `rom_dir` doesn't exist; a file that can't be read, or an I/O error while
    /// searching for one, is reported as [`Status::Unreadable`].
    pub fn audit(&self, rom_dir: &Path, system_dir: &Path) -> Result<GameAudit> {
        if !rom_dir.is_dir() {
            return Err(invalid(
                rom_dir.display().to_string(),
                "the ROM directory doesn't exist",
            ));
        }
        let mut files = Vec::new();
        let content = &self.content;
        let parents_dir: Option<PathBuf>;
        if content.in_parent {
            let parent = &content.parents[0];
            match find_file(rom_dir, parent) {
                Ok(path) => {
                    parents_dir = path.as_ref().map(|path| dir_of(path));
                    // The clone's members are in the parent's archive: audit both from one
                    // reading.
                    let archive = path.as_deref().map(read_archive);
                    let mut clone = archive_audit(Role::Content, content, path.clone(), &archive);
                    clone.extra.clear();
                    let claimed: HashSet<&str> = matched_names(&clone).collect();
                    let mut parent_audit = archive_audit(Role::Parent, parent, path, &archive);
                    parent_audit
                        .extra
                        .retain(|entry| !claimed.contains(entry.name.as_str()));
                    files.push(clone);
                    files.push(parent_audit);
                }
                Err(error) => {
                    parents_dir = None;
                    files.push(unreadable(Role::Content, content, &error));
                    files.push(unreadable(Role::Parent, parent, &error));
                }
            }
        } else {
            match find_file(rom_dir, content) {
                Ok(path) => {
                    let path = path.or_else(|| named(rom_dir, content));
                    parents_dir = path.as_ref().map(|path| dir_of(path));
                    files.push(file_audit(Role::Content, content, path));
                }
                Err(error) => {
                    parents_dir = None;
                    files.push(unreadable(Role::Content, content, &error));
                }
            }
        }
        let skip = usize::from(content.in_parent);
        for parent in &content.parents[skip..] {
            let path = parents_dir.as_ref().map(|dir| dir.join(&parent.file));
            files.push(file_audit(Role::Parent, parent, path));
        }
        for bios in &self.bios {
            // `game.toml` separates directories with `/`: join them one by one, so the path
            // uses the platform's separator throughout.
            let path =
                (bios.file.split('/')).fold(system_dir.to_path_buf(), |path, part| path.join(part));
            files.push(file_audit(Role::Bios, bios, Some(path)));
        }
        let status = (files.iter().map(|file| file.status))
            .max()
            .unwrap_or(Status::Ok);
        Ok(GameAudit { status, files })
    }
}

fn dir_of(path: &Path) -> PathBuf {
    path.parent().unwrap_or(Path::new("")).to_path_buf()
}

/// `dir/<spec.file>` if a file is there (a plain file with the wrong hashes), else `None`.
fn named(dir: &Path, spec: &FileSpec) -> Option<PathBuf> {
    let path = dir.join(&spec.file);
    path.is_file().then_some(path)
}

/// The archive entries an audit used: matched, or bad or unreadable under a member's name.
fn matched_names(audit: &FileAudit) -> impl Iterator<Item = &str> {
    (audit.members.iter())
        .filter(|member| member.status != Status::Missing)
        .map(|member| member.found_as.as_deref().unwrap_or(&member.name))
}

/// Audits a file at `path` (`None`, or a path with no file: missing).
fn file_audit(role: Role, spec: &FileSpec, path: Option<PathBuf>) -> FileAudit {
    let path = match path {
        Some(path) if path.is_file() => Ok(path),
        other => Err(other),
    };
    match (path, &spec.hashes) {
        (Ok(path), Some(hashes)) => plain_audit(role, spec, path, hashes),
        (Ok(path), None) => {
            let archive = read_archive(&path);
            archive_audit(role, spec, Some(path), &Some(archive))
        }
        (Err(looked_at), _) => FileAudit {
            path: looked_at,
            ..blank(role, spec, Status::Missing)
        },
    }
}

/// A file whose search failed with an I/O error.
fn unreadable(role: Role, spec: &FileSpec, error: &crate::Error) -> FileAudit {
    FileAudit {
        error: Some(error.to_string()),
        ..blank(role, spec, Status::Unreadable)
    }
}

fn blank(role: Role, spec: &FileSpec, status: Status) -> FileAudit {
    FileAudit {
        role,
        file: spec.file.clone(),
        status,
        path: None,
        found: None,
        members: Vec::new(),
        extra: Vec::new(),
        error: None,
    }
}

fn plain_audit(role: Role, spec: &FileSpec, path: PathBuf, hashes: &Hashes) -> FileAudit {
    let found = std::fs::File::open(&path).and_then(hash_reader);
    let mut audit = blank(role, spec, Status::Ok);
    match found {
        Ok(found) if found.matches(hashes.size, &hashes.crc32, &hashes.sha1) => {}
        Ok(found) => {
            audit.status = Status::BadHash;
            audit.found = Some(found);
        }
        Err(error) => {
            audit.status = Status::Unreadable;
            audit.error = Some(error.to_string());
        }
    }
    audit.path = Some(path);
    audit
}

/// An archive's members with their hashes, or why it couldn't be read.
type Archive = std::result::Result<Vec<EntryRead>, String>;

/// A member as read: its hashes, or why it couldn't be read.
struct EntryRead {
    name: String,
    found: std::result::Result<Found, String>,
}

/// Audits an archive's members against `spec.members` ([`audit_member`]). The archive is
/// `BadHash` if a member is missing or bad, `Unreadable` if one can't be read. `archive` is
/// `None` when the archive wasn't found.
fn archive_audit(
    role: Role,
    spec: &FileSpec,
    path: Option<PathBuf>,
    archive: &Option<Archive>,
) -> FileAudit {
    let mut audit = blank(role, spec, Status::Ok);
    audit.path = path;
    let entries = match archive {
        None => {
            audit.status = Status::Missing;
            return audit;
        }
        Some(Err(error)) => {
            audit.status = Status::Unreadable;
            audit.error = Some(error.clone());
            return audit;
        }
        Some(Ok(entries)) => entries,
    };
    let mut used = vec![false; entries.len()];
    for member in &spec.members {
        let result = audit_member(member, entries, &mut used);
        let status = match result.status {
            Status::Missing => Status::BadHash,
            status => status,
        };
        audit.status = audit.status.max(status);
        audit.members.push(result);
    }
    for (entry, used) in entries.iter().zip(used) {
        if !used {
            audit.extra.push(Entry {
                name: entry.name.clone(),
                found: entry.found.clone().ok(),
                error: entry.found.clone().err(),
            });
        }
    }
    audit
}

/// Matches an expected member to an archive entry by size, CRC-32 and SHA-1, whatever its name
/// (renamed, or in a merged set's `<clone>/`), preferring the entry with its name. Several
/// members may match the same entry (a set that lists one ROM twice). With no match, the entry
/// with its name, if any, makes it bad or unreadable; else it's missing. Marks the entries
/// matched in `used`, so the others are reported as extras.
fn audit_member(member: &Member, entries: &[EntryRead], used: &mut [bool]) -> MemberAudit {
    let mut audit = MemberAudit {
        name: member.name.clone(),
        status: Status::Ok,
        found_as: None,
        found: None,
        error: None,
    };
    let matches = |entry: &EntryRead| {
        (entry.found.as_ref())
            .is_ok_and(|found| found.matches(member.size, &member.crc32, &member.sha1))
    };
    let by_name = entries.iter().position(|entry| entry.name == member.name);
    let by_hash = (by_name.filter(|&index| matches(&entries[index])))
        .or_else(|| entries.iter().position(matches));
    if let Some(index) = by_hash {
        used[index] = true;
        if by_name != Some(index) {
            audit.found_as = Some(entries[index].name.clone());
        }
        return audit;
    }
    audit.status = match by_name {
        None => Status::Missing,
        Some(index) => {
            used[index] = true;
            match &entries[index].found {
                Ok(found) => {
                    audit.found = Some(found.clone());
                    Status::BadHash
                }
                Err(error) => {
                    audit.error = Some(error.clone());
                    Status::Unreadable
                }
            }
        }
    };
    audit
}

/// Reads every file in the zip archive at `path` and hashes it.
fn read_archive(path: &Path) -> Archive {
    let fail = |error: &dyn std::fmt::Display| format!("{}: {error}", path.display());
    let file = std::fs::File::open(path).map_err(|error| fail(&error))?;
    let mut buffer = vec![0; rawzip::RECOMMENDED_BUFFER_SIZE];
    let archive = rawzip::ZipArchive::from_file(file, &mut buffer).map_err(|error| fail(&error))?;
    let mut entries = archive.entries(&mut buffer);
    let mut read = Vec::new();
    while let Some(entry) = entries.next_entry().map_err(|error| fail(&error))? {
        if entry.is_dir() {
            continue;
        }
        let name = String::from_utf8_lossy(entry.file_path().as_ref()).into_owned();
        let found = archive
            .get_entry(entry.wayfinder())
            .map_err(|error| error.to_string())
            .and_then(|local| {
                let reader = local.reader();
                let result = match entry.compression_method() {
                    rawzip::CompressionMethod::STORE => hash_reader(local.verifying_reader(reader)),
                    rawzip::CompressionMethod::DEFLATE => hash_reader(
                        local.verifying_reader(flate2::read::DeflateDecoder::new(reader)),
                    ),
                    method => {
                        return Err(format!("unsupported compression method {method:?}"));
                    }
                };
                result.map_err(|error| error.to_string())
            });
        read.push(EntryRead { name, found });
    }
    Ok(read)
}

/// The size, CRC-32 and SHA-1 of everything `reader` yields.
fn hash_reader(mut reader: impl Read) -> std::io::Result<Found> {
    let mut sha1 = Sha1::new();
    let mut crc32 = crc32fast::Hasher::new();
    let mut size = 0;
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        sha1.update(&buffer[..read]);
        crc32.update(&buffer[..read]);
        size += read as u64;
    }
    Ok(Found {
        size,
        crc32: format!("{:08x}", crc32.finalize()),
        sha1: hex(&sha1.finalize()),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::roms::tests::TempDir;

    /// `len` pseudo-random bytes (xorshift64): stand-ins for ROM data.
    fn random(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    fn found(data: &[u8]) -> Found {
        Found {
            size: data.len() as u64,
            crc32: format!("{:08x}", crc32fast::hash(data)),
            sha1: hex(&Sha1::digest(data)),
        }
    }

    /// A zip of `(name, data, deflate)` entries.
    fn zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut output = Vec::new();
        let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
        for (name, data, deflate) in entries {
            let method = if *deflate {
                rawzip::CompressionMethod::DEFLATE
            } else {
                rawzip::CompressionMethod::STORE
            };
            let (mut entry, config) = archive
                .new_file(name)
                .compression_method(method)
                .start()
                .unwrap();
            if *deflate {
                let encoder =
                    flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
                let mut writer = config.wrap(encoder);
                writer.write_all(data).unwrap();
                let (encoder, descriptor) = writer.finish().unwrap();
                encoder.finish().unwrap();
                entry.finish(descriptor).unwrap();
            } else {
                let mut writer = config.wrap(&mut entry);
                writer.write_all(data).unwrap();
                let (_, descriptor) = writer.finish().unwrap();
                entry.finish(descriptor).unwrap();
            }
        }
        archive.finish().unwrap();
        output
    }

    fn member(name: &str, data: &[u8]) -> String {
        let Found { size, crc32, sha1 } = found(data);
        format!("{{ name = \"{name}\", size = {size}, sha1 = \"{sha1}\", crc32 = \"{crc32}\" }}")
    }

    fn plain(data: &[u8]) -> String {
        let Found { size, crc32, sha1 } = found(data);
        format!("size = {size}\nsha1 = \"{sha1}\"\ncrc32 = \"{crc32}\"")
    }

    fn config(content: &str, bios: &str) -> GameConfig {
        let text = format!(
            "core = \"c\"\norientation = 0\nbuttons = {{ a = \"shot\" }}\n\
             reset = {{ script = [{{ press = [], frames = 1 }}] }}\n\
             episode = {{ terminate = \"0\" }}\n[content]\n{content}\n{bios}"
        );
        GameConfig::parse(&text, "test").unwrap()
    }

    fn statuses(audit: &FileAudit) -> Vec<(&str, Status)> {
        (audit.members.iter())
            .map(|member| (member.name.as_str(), member.status))
            .collect()
    }

    #[test]
    fn hashes_match_the_standard_test_vectors() {
        let abc = hash_reader(&b"abc"[..]).unwrap();
        assert_eq!(abc.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!((abc.crc32.as_str(), abc.size), ("352441c2", 3));
    }

    #[test]
    fn plain_files_are_ok_bad_or_missing() {
        let dir = TempDir::new("audit-plain");
        let (roms, system) = (dir.0.join("roms"), dir.0.join("system"));
        std::fs::create_dir_all(roms.join("sub")).unwrap();
        let (game, bios) = (random(1, 4000), random(2, 300));
        let game_toml = config(
            &format!("file = \"game.bin\"\n{}", plain(&game)),
            &format!("[[bios]]\nfile = \"sub/bios.bin\"\n{}", plain(&bios)),
        );
        let audit = game_toml.audit(&roms, &system).unwrap();
        assert_eq!(audit.status, Status::Missing, "nothing present is bad");
        assert_eq!(audit.files[0].status, Status::Missing);
        assert_eq!(audit.files[0].path, None, "searched, so no one place");
        assert_eq!(audit.files[1].status, Status::Missing);
        assert_eq!(audit.files[1].path, Some(system.join("sub/bios.bin")));

        std::fs::write(roms.join("sub/Renamed Game.bin"), &game).unwrap();
        std::fs::create_dir_all(system.join("sub")).unwrap();
        std::fs::write(system.join("sub/bios.bin"), &bios).unwrap();
        let audit = game_toml.audit(&roms, &system).unwrap();
        assert_eq!(audit.status, Status::Ok, "{audit:#?}");
        assert_eq!(audit.files[0].path, Some(roms.join("sub/Renamed Game.bin")));
        assert_eq!(
            (audit.files[0].role, audit.files[1].role),
            (Role::Content, Role::Bios)
        );

        let mut bad = bios.clone();
        bad[7] ^= 1;
        std::fs::write(system.join("sub/bios.bin"), &bad).unwrap();
        std::fs::remove_file(roms.join("sub/Renamed Game.bin")).unwrap();
        std::fs::write(roms.join("game.bin"), &game[1..]).unwrap();
        let audit = game_toml.audit(&roms, &system).unwrap();
        assert_eq!(
            audit.files[0].status,
            Status::BadHash,
            "by name, wrong bytes"
        );
        assert_eq!(audit.files[0].found, Some(found(&game[1..])));
        assert_eq!(audit.files[1].status, Status::BadHash);
        assert_eq!(audit.files[1].found, Some(found(&bad)));
        assert_eq!(audit.status, Status::BadHash);
        assert!(game_toml.audit(&dir.0.join("nope"), &system).is_err());
    }

    #[test]
    fn archive_members_are_checked_by_hash_then_by_name() {
        let dir = TempDir::new("audit-archive");
        let roms = dir.0.join("roms");
        std::fs::create_dir_all(&roms).unwrap();
        let data: Vec<Vec<u8>> = (10..16).map(|seed| random(seed, 5000)).collect();
        let game_toml = config(
            &format!(
                "file = \"set.zip\"\nmembers = [{}, {}, {}, {}, {}]",
                member("ok.bin", &data[0]),
                member("stored.bin", &data[1]),
                member("renamed.bin", &data[2]),
                member("bad.bin", &data[3]),
                member("missing.bin", &data[4]),
            ),
            "",
        );
        let mut bad = data[3].clone();
        bad[0] ^= 0xff;
        let archive = zip(&[
            ("ok.bin", &data[0], true),
            ("stored.bin", &data[1], false),
            ("set/other-name.bin", &data[2], true),
            ("bad.bin", &bad, true),
            ("extra.bin", &data[5], true),
        ]);
        std::fs::write(roms.join("set.zip"), &archive).unwrap();
        let audit = game_toml.audit(&roms, &dir.0).unwrap();
        let file = &audit.files[0];
        assert_eq!(
            (audit.status, file.status),
            (Status::BadHash, Status::BadHash)
        );
        assert_eq!(
            statuses(file),
            [
                ("ok.bin", Status::Ok),
                ("stored.bin", Status::Ok),
                ("renamed.bin", Status::Ok),
                ("bad.bin", Status::BadHash),
                ("missing.bin", Status::Missing),
            ]
        );
        assert_eq!(
            file.members[2].found_as.as_deref(),
            Some("set/other-name.bin")
        );
        assert_eq!(file.members[3].found, Some(found(&bad)));
        let extra = Entry {
            name: "extra.bin".into(),
            found: Some(found(&data[5])),
            error: None,
        };
        assert_eq!(file.extra, [extra]);

        let good = zip(&[
            ("ok.bin", &data[0], true),
            ("stored.bin", &data[1], false),
            ("renamed.bin", &data[2], true),
            ("bad.bin", &data[3], true),
            ("missing.bin", &data[4], false),
        ]);
        std::fs::write(roms.join("set.zip"), &good).unwrap();
        let audit = game_toml.audit(&roms, &dir.0).unwrap();
        assert_eq!(audit.status, Status::Ok, "{audit:#?}");
        assert!(audit.files[0].extra.is_empty());

        // A stored member whose bytes don't match its CRC in the archive.
        let mut corrupt = good.clone();
        let at = (corrupt.windows(64))
            .position(|window| window == &data[1][..64])
            .unwrap();
        corrupt[at] ^= 1;
        std::fs::write(roms.join("set.zip"), &corrupt).unwrap();
        let audit = game_toml.audit(&roms, &dir.0).unwrap();
        let member = &audit.files[0].members[1];
        assert_eq!(member.status, Status::Unreadable, "{member:?}");
        assert!(member.error.is_some());
        assert_eq!(audit.files[0].status, Status::Unreadable);
        assert_eq!(audit.status, Status::Unreadable, "the worst file status");

        std::fs::write(roms.join("set.zip"), b"not a zip").unwrap();
        let audit = game_toml.audit(&roms, &dir.0).unwrap();
        assert_eq!(audit.files[0].status, Status::Unreadable);
        assert!(audit.files[0].error.as_ref().unwrap().contains("set.zip"));
    }

    #[test]
    fn members_match_swapped_or_shared_entries() {
        let dir = TempDir::new("audit-swapped");
        let roms = dir.0.join("roms");
        std::fs::create_dir_all(&roms).unwrap();
        let (a, b) = (random(40, 700), random(41, 700));
        let game_toml = config(
            &format!(
                "file = \"set.zip\"\nmembers = [{}, {}, {}]",
                member("a.bin", &a),
                member("b.bin", &b),
                member("a-again.bin", &a),
            ),
            "",
        );
        // a.bin and b.bin hold each other's bytes; a-again.bin is a.bin's bytes, stored once.
        let archive = zip(&[
            ("a.bin", &b, true),
            ("b.bin", &a, true),
            ("junk.bin", &[1], false),
        ]);
        std::fs::write(roms.join("set.zip"), archive).unwrap();
        let audit = game_toml.audit(&roms, &dir.0).unwrap();
        let file = &audit.files[0];
        assert_eq!(audit.status, Status::Ok, "{audit:#?}");
        let found_as: Vec<_> = (file.members.iter())
            .map(|member| member.found_as.as_deref())
            .collect();
        assert_eq!(found_as, [Some("b.bin"), Some("a.bin"), Some("b.bin")]);
        let extra: Vec<_> = file.extra.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(extra, ["junk.bin"]);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_doesnt_stop_the_audit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("audit-unreadable");
        let roms = dir.0.join("roms");
        std::fs::create_dir_all(&roms).unwrap();
        let game = random(50, 100);
        let game_toml = config(&format!("file = \"game.bin\"\n{}", plain(&game)), "");
        // A file of the content's size, which the search must hash, but can't; the content
        // isn't elsewhere, so the error is reported.
        let locked = roms.join("locked.bin");
        std::fs::write(&locked, random(51, 100)).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads it anyway.
        let is_locked = std::fs::File::open(&locked).is_err();
        let audit = game_toml.audit(&roms, &dir.0);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
        if !is_locked {
            return;
        }
        let audit = audit.unwrap();
        assert_eq!(audit.status, Status::Unreadable, "{audit:#?}");
        assert!(
            audit.files[0]
                .error
                .as_ref()
                .unwrap()
                .contains("locked.bin")
        );
    }

    #[test]
    fn a_merged_clone_is_audited_inside_its_parent() {
        let dir = TempDir::new("audit-merged");
        let (roms, system) = (dir.0.join("roms"), dir.0.join("system"));
        std::fs::create_dir_all(roms.join("arcade")).unwrap();
        std::fs::create_dir_all(system.join("fbneo")).unwrap();
        let data: Vec<Vec<u8>> = (20..25).map(|seed| random(seed, 3000)).collect();
        let game_toml = config(
            &format!(
                "file = \"clone.zip\"\nin_parent = true\nmembers = [{}, {}]\n\
                 parents = [{{ file = \"parent.zip\", members = [{}, {}] }}]",
                member("clone.bin", &data[0]),
                member("shared.bin", &data[2]),
                member("parent.bin", &data[1]),
                member("shared.bin", &data[2]),
            ),
            &format!(
                "[[bios]]\nfile = \"fbneo/bios.zip\"\nmembers = [{}]",
                member("bios.bin", &data[3])
            ),
        );
        let audit = game_toml.audit(&roms, &system).unwrap();
        let roles: Vec<_> = audit
            .files
            .iter()
            .map(|file| (file.role, file.status))
            .collect();
        assert_eq!(
            roles,
            [
                (Role::Content, Status::Missing),
                (Role::Parent, Status::Missing),
                (Role::Bios, Status::Missing)
            ]
        );
        let parent = zip(&[
            ("parent.bin", &data[1], true),
            ("shared.bin", &data[2], true),
            ("clone/clone.bin", &data[0], true),
            ("other/clone.bin", &data[4], true),
        ]);
        std::fs::write(roms.join("arcade/parent.zip"), parent).unwrap();
        std::fs::write(
            system.join("fbneo/bios.zip"),
            zip(&[("bios.bin", &data[3], true)]),
        )
        .unwrap();
        let audit = game_toml.audit(&roms, &system).unwrap();
        assert_eq!(audit.status, Status::Ok, "{audit:#?}");
        let (clone, parent) = (&audit.files[0], &audit.files[1]);
        assert_eq!(clone.path, Some(roms.join("arcade/parent.zip")));
        assert_eq!(
            clone.members[0].found_as.as_deref(),
            Some("clone/clone.bin")
        );
        assert!(
            clone.extra.is_empty(),
            "extras are reported once, by the parent"
        );
        let extra: Vec<_> = parent.extra.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(extra, ["other/clone.bin"], "not the clone's members");

        // A bad clone member under its own name is reported on the clone only.
        let parent = zip(&[
            ("parent.bin", &data[1], true),
            ("shared.bin", &data[2], true),
            ("clone.bin", &data[4], true),
        ]);
        std::fs::write(roms.join("arcade/parent.zip"), parent).unwrap();
        let audit = game_toml.audit(&roms, &system).unwrap();
        let (clone, parent) = (&audit.files[0], &audit.files[1]);
        assert_eq!(clone.members[0].status, Status::BadHash);
        assert_eq!(
            (parent.status, parent.extra.len()),
            (Status::Ok, 0),
            "{parent:#?}"
        );
    }

    #[test]
    fn split_parents_are_checked_beside_the_content() {
        let dir = TempDir::new("audit-split");
        let roms = dir.0.join("roms");
        std::fs::create_dir_all(roms.join("arcade")).unwrap();
        let data = random(30, 100);
        let game_toml = config(
            &format!(
                "file = \"clone.zip\"\nmembers = [{}]\nparents = [{{ file = \"parent.zip\", \
                 members = [{}] }}]",
                member("a.bin", &data),
                member("a.bin", &data),
            ),
            "",
        );
        let archive = zip(&[("a.bin", &data, true)]);
        std::fs::write(roms.join("arcade/clone.zip"), &archive).unwrap();
        std::fs::write(roms.join("parent.zip"), &archive).unwrap();
        let audit = game_toml.audit(&roms, &dir.0).unwrap();
        assert_eq!(audit.files[0].status, Status::Ok);
        assert_eq!(
            audit.files[1].status,
            Status::Missing,
            "not beside the content"
        );
        assert_eq!(audit.files[1].path, Some(roms.join("arcade/parent.zip")));
        std::fs::write(roms.join("arcade/parent.zip"), &archive).unwrap();
        assert_eq!(game_toml.audit(&roms, &dir.0).unwrap().status, Status::Ok);
    }

    #[test]
    fn the_audit_serializes_to_json() {
        let dir = TempDir::new("audit-json");
        let game_toml = config(&format!("file = \"g.bin\"\n{}", plain(b"abc")), "");
        let json = game_toml.audit(&dir.0, &dir.0).unwrap().to_json();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "missing");
        assert_eq!(value["files"][0]["role"], "content");
        assert_eq!(value["files"][0]["status"], "missing");
        assert_eq!(value["files"][0]["file"], "g.bin");
        assert!(value["files"][0].get("members").is_none(), "{json}");
    }
}
