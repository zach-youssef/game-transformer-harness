# ROMs, BIOS and cores

The harness runs games from ROM and BIOS files that the user supplies from their own dumps.

## Rules

- Never download ROMs or BIOS files, and never ask a tool or service to fetch them.
- Never commit, upload or share them. That includes test fixtures, CI caches, release assets and
  logs that quote their contents.
- Core binaries (`.so`, `.dylib`, `.dll`) are build outputs and are never committed either.
- Tests that need a ROM skip with a reason when it isn't there. CI has no ROMs and runs every
  test against `gth-testcore` instead.

`.gitignore` enforces this for the usual extensions (`.zip`, `.7z`, `.chd`, `.cue`, `.bin`,
`.iso`, `.pce`, `.nes`, `.gen`, `.smd`, `.inp`) and for the root directories below. Genesis
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
audited by hash in P2. Symlinked directories aren't followed.

The content's `parents` (an arcade clone's parent set) must be in the directory where the
content was found, where the core looks for them: `GameConfig::missing_parents` checks them
there by name, and by size and SHA-1 when given. `GameConfig::missing_bios` checks each BIOS
at its path under the system directory (`fbneo/pgm.zip` is `system/fbneo/pgm.zip`).
`GameConfig::find_content` puts these together: for content `in_parent` (a clone in a
merged set) it finds the first parent instead and returns the clone's path beside it.

## Data directory

`RetroEnv.from_game(game_id, data_dir, ...)` takes everything but the content from a data
directory (the git-ignored `data/` in a checkout, or anywhere else):

```
<data_dir>/
  system/                 BIOS files at their game.toml paths, checked by name and SHA-1
                          (an archive by name); e.g. fbneo/pgm.zip
  cores/                  <core>_libretro.<so|dylib|dll>, unless core_path is given
  states/<game-id>/       reset savestates (game.toml `reset.state`, checked by SHA-1)
```

## Tests

Tests that need a user's ROM take the `rom` fixture (`tests/conftest.py`): `rom(game_id)`
returns the game's content path, found as above, and skips the test with a reason when the
ROM directory is unset or the file isn't in it. A `GTH_ROM_DIR` (or `rom_dir`) that names a
directory that doesn't exist is a misconfiguration, not a missing ROM: the fixture raises
`ConfigError` rather than skipping, so the mistake is seen. Tests of the lookup itself use
`games/testcore`, whose files the `testcore_data` fixture writes to a temporary directory.

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
match what the pinned core expects is checked by hash in P2's audit, not assumed from the
name.

`roms/`, `system/`, `cores/` and `runs/` at the repo root are ignored as well, so a stray copy
there is never committed.
