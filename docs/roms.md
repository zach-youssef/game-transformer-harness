# ROMs, BIOS and cores

The harness runs games from ROM and BIOS files that the user supplies from their own dumps.

## Rules

- Never download ROMs or BIOS files, and never ask a tool or service to fetch them.
- Never commit, upload or share them. That includes test fixtures, CI caches, release assets and
  logs that quote their contents.
- Core binaries (`.so`, `.dylib`, `.dll`) are build outputs and are never committed either.
  The cores workflow builds and publishes them; get them with `gth cores fetch`
  (docs/cores.md).
- Tests that need a ROM skip with a reason when it isn't there. CI has no ROMs and runs every
  test against `gth-testcore` instead.

`.gitignore` enforces this for the usual extensions (`.zip`, `.7z`, `.chd`, `.cue`, `.bin`,
`.iso`, `.pce`, `.nes`, `.gen`, `.smd`, `.inp`), for savestates (`.state`: made from a user's
ROM, such as the ones a RAM search script loads) and for the root directories below. Genesis
ROMs ending in `.md` can't be ignored by extension, so keep them inside `data/`.

## Where files live

Each machine keeps its ROMs in one directory outside the repo:

| Machine | ROM directory |
|---|---|
| Mac | `~/roms` (`manifest.md` there lists names, sizes and hashes) |
| TheaterPC | `D:\research\roms` |

The harness finds that directory through the `GTH_ROM_DIR` environment variable or `rom_dir` in
the user config file, never through a hard-coded path, and identifies files by SHA-1 rather
than by name.

## Finding files

`gth_config::rom_dir()` returns the ROM directory:

1. `GTH_ROM_DIR`, if set and not empty;
2. else `rom_dir` in the user config file, a TOML file whose only key is `rom_dir` (a
   relative path is relative to the file). The file is `GTH_CONFIG` if set, else
   `%APPDATA%\gtharness\config.toml` on Windows and
   `$XDG_CONFIG_HOME/gtharness/config.toml` (default `~/.config/...`) elsewhere;
3. else none: tests that need a ROM skip.

`gth_config::find_file(rom_dir, spec)` finds a game's `content` (docs/games.md) under the ROM
directory and its subdirectories: the file named `spec.file` if its size and SHA-1 match,
else any file with that size and SHA-1, so renamed dumps are found. Only files of the right
size are hashed. An archive described by `members` is found by name; its members are
checked by the audit (below). Symlinked directories aren't followed. A subdirectory or file
that can't be read is skipped; if the file isn't found elsewhere, the lookup fails with that
error (the audit reports the file `unreadable`).

The content's `parents` (an arcade clone's parent set) must be in the directory where the
content was found, where the core looks for them: `GameConfig::missing_parents` checks them
there by name, and by size and SHA-1 when given. `GameConfig::missing_bios` checks each BIOS
at its path under the system directory (`fbneo/pgm.zip` is `system/fbneo/pgm.zip`).
`GameConfig::find_content` puts these together: for content `in_parent` (a clone in a
merged set) it finds the first parent instead and returns the clone's path beside it.

## Auditing

`gth audit <data_dir>` checks every game in `games/` (`--games-dir` for another directory),
but those whose `core` is the test core unless named with `--game`, against the user's files,
read-only:

- the content, found as above under the ROM directory (`--rom-dir`, else `GTH_ROM_DIR` or
  `rom_dir`), and its parents beside it;
- the BIOS files at their paths under `<data_dir>/system`;
- every member of each archive, by size, CRC-32 and SHA-1. A member matches any entry with
  those, whatever its name (preferring its own name), since MAME's merged sets keep clone
  files in `<clone>/` and FBNeo finds members by CRC whatever their name; several members may
  match one entry (a ROM listed twice). With no match, the entry with its name, if any, is
  reported as bad or unreadable: `<set>/<name>` first, where `<set>` is the archive's name
  without its extension, so a bad clone member in a merged set is `bad-hash`, not
  `missing`; else `<name>`. A clone `in_parent` is audited inside its parent's archive.

Each file and member is `ok`, `missing`, `bad-hash` (the size or a hash differs; for an
archive, a member is missing or bad) or `unreadable` (an I/O error, also while searching the
ROM directory; a zip that doesn't parse; a member that fails its own CRC or uses a
compression method other than store and deflate). Content with the wrong hashes is found by
its name at the top of the ROM directory only; elsewhere it's `missing`. The report gives the
size and hashes found for anything bad, and lists the archive members that match no expected
member (not an error: a merged set holds its clones), with their hashes or error. A game's
status is the worst of its files', from best to worst `ok`, `missing`, `bad-hash`,
`unreadable`: a game whose files are absent is `missing`, one with a bad dump `bad-hash`. It
prints a table, or with `--json` the report as JSON:

```json
{"rom_dir": "...", "system_dir": "...", "games": {"<game-id>": {"status": "ok", "files": [
  {"role": "content | parent | bios", "file": "<game.toml name>", "status": "ok",
   "path": "...", "found": {"size": 0, "crc32": "...", "sha1": "..."},
   "members": [{"name": "...", "status": "ok", "found_as": "<clone>/...", "found": {}}],
   "extra": [{"name": "...", "size": 0, "crc32": "...", "sha1": "...", "error": "..."}],
   "error": "..."}]}}}
```

Optional keys appear only when they apply; the table counts the other members and how many
of them are unreadable. The exit status is 0 when every audited game is ok, 1 otherwise and 2
on a usage or config error (no ROM directory, one that doesn't exist, or no game to audit).
The work is `GameConfig::audit` in `gth-config` (`audit.rs`); the CLI is
`python/gtharness/cli.py`.

### Content tables

| Game | `games/<id>` | Hashes from |
|---|---|---|
| Gunhed | `gunhed` | the user's dump (`~/roms/manifest.md`); the rest of the config: `games/gunhed/README.md` |
| Zanac | `zanac` | the user's dump (`~/roms/manifest.md`); the rest of the config: `games/zanac/README.md` |
| Lightening Force | `lightening-force` | the user's dump (`~/roms/manifest.md`); only `[content]` is final until G3 |
| Battle Garegga, Ketsui, DoDonPachi III and DoDonPachi Dai-Ou-Jou Black Label, Gunbird, Strikers 1945 | `bgaregga`, `ket`, `ddp3`, `ddpdojblk` (in `ddp3.zip`, `in_parent`), `gunbird`, `s1945` | names, sizes and CRC-32s from the pinned FBNeo's driver tables (`a836e52`, below); SHA-1s from MAME 0.289 (below); only `[content]` (and `[buttons]` for `gunbird` and `s1945`) is final until G4-G6 |

FBNeo's driver tables list each member's name, size and CRC-32 but no SHA-1, which `members`
needs, so the arcade tables take each member's name, size and CRC-32 from FBNeo at the
`cores.lock` pin (upstream `a836e52`, which our fork's commits leave alone, docs/cores.md),
the core that loads them, and its SHA-1 from MAME's driver for the same set, used only where
MAME's CRC-32 and size match FBNeo's:

| Sets | FBNeo `a836e52` | MAME 0.289 |
|---|---|---|
| `bgaregga` | [`toaplan/d_battleg.cpp`](https://github.com/libretro/FBNeo/blob/a836e52198aa5ce1f54be7b050e44872a1223797/src/burn/drv/toaplan/d_battleg.cpp) | [`toaplan/raizing.cpp`](https://github.com/mamedev/mame/blob/mame0289/src/mame/toaplan/raizing.cpp) |
| `ket`, `ddp3`, `ddpdojblk` | [`pgm/d_pgm.cpp`](https://github.com/libretro/FBNeo/blob/a836e52198aa5ce1f54be7b050e44872a1223797/src/burn/drv/pgm/d_pgm.cpp) | [`igs/pgm.cpp`](https://github.com/mamedev/mame/blob/mame0289/src/mame/igs/pgm.cpp) |
| `gunbird`, `s1945` | [`psikyo/d_psikyo.cpp`](https://github.com/libretro/FBNeo/blob/a836e52198aa5ce1f54be7b050e44872a1223797/src/burn/drv/psikyo/d_psikyo.cpp) | [`psikyo/psikyo.cpp`](https://github.com/mamedev/mame/blob/mame0289/src/mame/psikyo/psikyo.cpp) |

MAME 0.289 is the latest MAME release before the FBNeo pin (2026-07-30; the pin is from
2026-11-10), so its tables are the closest to what FBNeo follows, and the user's sets are MAME
merged sets. Every member's CRC-32 and size agree between the two, so no member lacks a SHA-1.
A set's table is FBNeo's driver table plus the BIOS table it adds (`STDROMPICKEXT`): the
PGM sets carry their BIOS ROMs inside their own zip, so they have no `[[bios]]`. Members
that were never dumped (`NODUMP` in both drivers: the PGM sets' `*_igs027a.bin`, Gunbird's
`3020.u19` and `3021.u69`, Strikers 1945's `4-u59.bin`) are left out, as no set holds them.
When the FBNeo pin moves, compare its tables again (docs/cores.md, "Bumping a pin").

## Data directory

`RetroEnv.from_game(game_id, data_dir, ...)` takes everything but the content from a data
directory (the git-ignored `data/` in a checkout, or anywhere else):

```
<data_dir>/
  system/                 BIOS files at their game.toml paths, checked by name and SHA-1
                          (an archive by name); e.g. fbneo/pgm.zip
  cores/                  <core>_libretro.<so|dylib|dll>, unless core_path is given;
                          `gth cores fetch <data_dir>` puts the pinned builds here
  states/<game-id>/       reset savestates (game.toml `reset.state`, checked by SHA-1)
```

## Tests

Tests that need a user's ROM take the `rom` fixture, which skips without the ROM
(docs/testing.md, "ROM tests"). Tests of the lookup itself use `games/testcore`, whose files
the `testcore_data` fixture writes to a temporary directory.

## Local copies

Inside a checkout, the git-ignored `data/` directory holds local copies when a layout is needed,
for example for arcade sets that a core loads by set name:

```
data/
  roms/          content: <set>.zip for arcade, cartridge files, CD images (.cue + tracks or .chd)
  system/        BIOS files, e.g. syscard3.pce
    fbneo/       FBNeo BIOS zips, e.g. pgm.zip
```

Arcade sets are MAME zips named after the set: non-merged, split or merged all work with
FBNeo (docs/games.md, "Content and BIOS files"). A non-merged set is one zip. A split clone
has its own zip and lists its parent in `content.parents`, kept beside it. A merged clone
(`ddpdojblk` inside `ddp3.zip`) has no zip of its own: its content is `in_parent`, and the
harness finds it through the parent. Whether a set's contents
match what the pinned core expects is checked by hash in the audit (above), not assumed from
the name.

`roms/`, `system/`, `cores/` and `runs/` at the repo root are ignored as well, so a stray copy
there is never committed.
