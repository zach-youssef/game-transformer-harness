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
than by name. (Not implemented yet: ROM lookup lands with the game config, stack PR S5.)

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
