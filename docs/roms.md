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

Inside a checkout, the git-ignored `data/` directory holds local copies when a layout is needed,
for example for arcade sets that a core loads by set name:

```
data/
  roms/          content: <set>.zip for arcade, cartridge files, CD images (.cue + tracks or .chd)
  system/        BIOS files, e.g. syscard3.pce
    fbneo/       FBNeo BIOS zips, e.g. pgm.zip
```

Arcade sets are MAME zips named after the set, and merged sets are fine: a merged parent zip
also holds its clones (for example `ddp3.zip` holds `ddpdojblk`), so a clone needs no zip of
its own. Whether a set's contents match what the pinned core expects is checked by hash, not
assumed from the name.

`roms/`, `system/`, `cores/` and `runs/` at the repo root are ignored as well, so a stray copy
there is never committed.
