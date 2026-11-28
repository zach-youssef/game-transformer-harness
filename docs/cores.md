# Cores

The harness runs libretro cores built from pinned commits: upstream's, or for a core the
harness needs changes in, a commit on a branch of our fork of it (Forks, below).
`cores.lock` (repo root, schema in docs/games.md) pins each core's repository, commit, the
`library_version` the build reports and the SHA-256 of each platform's build. The cores
workflow builds them; `gth cores fetch` puts this platform's builds in a data directory and
checks them.

## Pins

Each pin is the newest commit on the upstream default branch's first-parent history that was
at least two weeks old when pinned: recent fixes, but past the window in which upstream
usually reverts or fixes up a change. First-parent, because a merge of upstream emulator
history brings in commits whose tree isn't the libretro port (FBNeo's `301a167` has no
`src/burner/libretro`).

| Core | Games | Repository | Commit | Date | Subject |
|---|---|---|---|---|---|
| `mednafen_pce_fast` | Gunhed | libretro/beetle-pce-fast-libretro | `2de1ce1c3f237b161c126c54ba407990bb86d1e9` | 2026-10-30 | Fetch translations & Recreate libretro_core_options_intl.h |
| `fceumm` | Zanac | libretro/libretro-fceumm | `e0f3086a8104f8fbbe49a2615b10401504a9533f` | 2026-11-02 | libretro: send path checks through the frontend VFS (#660) |
| `genesis_plus_gx` | Lightening Force | libretro/Genesis-Plus-GX | `b9c2316441978d4015ba30e9a95d9b0e6f8ee981` | 2026-11-09 | libretro: define ALIGN_LONG for 32-bit ARM unix builds (#415) |
| `ale` | Atari 2600 games ALE supports | zach-youssef/Arcade-Learning-Environment (`ale-core`) | `ef6d7ffe2159cd2e3a7d28891961ec17a12730d4` | 2026-11-27 | Build and test the libretro core in CI on Linux, macOS and Windows |
| `fbneo` | Battle Garegga, Ketsui, DoDonPachi III / Dai-Ou-Jou Black Label, Gunbird, Strikers 1945 | zach-youssef/FBNeo (`gth`) | `9ee23cb3fa517959b5e6cb2fd488564bcf5d0fbb` | 2026-11-27 | psikyo: give the sprite list scan variables unique area names |

FBNeo's pin is the head of branch `gth` of our fork: libretro/FBNeo's
`a836e52198aa5ce1f54be7b050e44872a1223797` (2026-11-10, "(libretro) update files", picked by
the rule above) and twenty-two commits. Twenty add state its savestates left out (Forks, below,
and docs/performance.md, "FBNeo and savestates"): for Battle Garegga `burn_ym2151`
(`ddb1e8c`), `toaplan` (`904777f`) and `msm6295` (`9fbf243`), for the Psikyo games `psikyo`
(`b061823`), `burn_ym2610` (`ad94ca3`), `burn_ymf278b` (`e85680d`) and `fm.c` (`f824e12`,
`e803e5d`), and for the IGS PGM games `pgm` (`9f55031`). The other eleven fix the same kind of
gap in code none of our FBNeo games runs (found by reading, and for `burn_md2612` on FBNeo's
Mega Drive driver): the sound chips' wrappers `burn_md2612` (`9064755`), `burn_ym2203`
(`69a003b`), `burn_ym2608` (`30be273`), `burn_ym2612` (`48cdea5`), `burn_ym3812` (`ac75c5f`),
`burn_ym3526` (`4d9384c`) and `burn_y8950` (`8e4de79`), the YM2608's FM core (`fm.c`,
`b3545fa`), the Y8950's ADPCM unit (`fmopl`, `21ab620`) and two PGM protection devices, the
4-in-1 bootleg's ROM bank (`pgm`, `c5866a1`) and Puzzli 2's simulation (`pgm`, `d09bde2`).
The last two expose the named state areas the sprite labels are read from (`8ece753`,
`9ee23cb`: Forks, below).

ALE's pin is the head of branch `ale-core` of our fork: ALE's release `v0.12.1`
(`8a8fafb1`, 2026-08-16) rather than the rule above, since ale-py (the Python package the
published ALE baselines ran) ships releases and v0.12.1 is the newest one, and three commits
(Forks, below): the ROM files and ROM-fetching scripts and workflows removed, the libretro
core (`libretro/`, README there) and its CI workflow.

FBNeo's upstream commit also fixes its driver tables, so the arcade sets' `game.toml` member
lists (docs/roms.md, "Content tables") are taken from FBNeo at
`a836e52198aa5ce1f54be7b050e44872a1223797` until FBNeo is bumped.

## Fetching

```
gth cores fetch data                 # the lock's GitHub release
gth cores fetch data --from DIR|URL  # a directory or URL with the release assets
```

For each core, `fetch` copies `<name>_libretro.<target>.<ext>` from the source into
`<data_dir>/cores/<name>_libretro.<ext>` (where `RetroEnv.from_game` looks, docs/roms.md),
checks its SHA-256 against the lock, then loads it without a game (`retro_init`,
`retro_get_system_info`, `retro_deinit`) and checks the `library_version` it reports. A build
with the wrong hash is never put in place, and a file that already has the pinned hash isn't
fetched again. Exit status: 0 when every core passes, 1 when one fails a check, 2 on a usage
or config error, including a lock without this platform's hashes (before its release, below).
`--lock PATH` (before `fetch`) uses another lock; the default is the checkout's, so outside a
checkout or editable install pass one. It never fetches ROMs or BIOS files.

Builds are for `aarch64-apple-darwin` (macOS arm64), `x86_64-pc-windows-msvc` (Windows x64:
the cores are MinGW builds, but a DLL's C ABI is the same, and the key is the harness's own
target) and `x86_64-unknown-linux-gnu`. `gth cores` takes this platform's triple from
`_native.build_info()`.

Loading a game checks nothing about the core: a core built some other way loads and runs if
its file is in `cores/` or passed as `core_path`, but its savestates and recordings won't
match a pinned build's. A recording keeps the core's lock entry (`[core.lock]`,
docs/recording-format.md) only when the core file has the pinned hash
(`gth_config::CoresLock::pinned`): `RetroEnv` checks it against the checkout's lock, and
`gth-play` against its `--lock` (docs/human-play.md).

## Builds

`scripts/build-core.sh <name> <repo> <commit> <osx|unix|win> <out_dir>` builds one
core with its own Makefile (ALE: CMake, below), upstream's route on every platform (on Windows in an MSYS2 MINGW64 shell
with MinGW-w64 gcc; the MSVC Makefile targets hard-code VS2017 and still need MSYS make).
Each core's Makefile and its fixes are a `case` in the script:

- The source is a shallow clone of exactly the commit with `core.abbrev` 7: cores put `git
  rev-parse --short HEAD` into `library_version`, so it must not depend on the clone.
- `mednafen_pce_fast`: its Makefile passes `GIT_VERSION` to C++ only, but `library_version`
  is set in C, so upstream builds report no commit; the script passes it to C too. On macOS
  its bundled zlib 1.2.11 defines `fdopen` away and clashes with the SDK's `<stdio.h>`, so the
  script defines `fdopen` as itself.
- `fbneo`: its Makefile dates the build with GNU `date -d` (missing on macOS) in local time,
  so the script passes the commit's date in UTC (`GIT_DATE`) and every platform reports the
  same version.
- `ale`: CMake rather than a Makefile, configuring ALE's C++ library without its Python
  bindings and building the `ale_libretro` target. ALE's CMake takes zlib from vcpkg when
  `VCPKG_ROOT` or `VCPKG_INSTALLATION_ROOT` is set, as on CI runners: static on Linux and
  macOS, but a DLL in vcpkg's `x64-windows` triplet. So on Windows the script hides vcpkg and
  uses MSYS2's cmake and static zlib (`-G "MSYS Makefiles" -DZLIB_USE_STATIC_LIBS=ON`; the
  workflow installs `mingw-w64-x86_64-cmake` and `mingw-w64-x86_64-zlib`). The core's CMake puts
  `git rev-parse --short=7 HEAD` into `library_version` (`0.12.1 ef6d7ff`); ALE's own version
  macro has no commit for an out-of-tree build.

Local builds work on a Mac with the Xcode command-line tools, for example to try a bump:

```
scripts/build-core.sh fceumm https://github.com/libretro/libretro-fceumm <commit> osx out
scripts/build-core.sh fbneo https://github.com/zach-youssef/FBNeo <commit> osx out
scripts/build-core.sh fbneo /abs/path/to/FBNeo <commit> osx out   # a local clone, unpushed
scripts/build-core.sh ale /abs/path/to/Arcade-Learning-Environment <commit> osx out   # needs cmake
gth cores check out          # loads every core in out/ and checks its version
```

A local build won't have the release's hash (the toolchain differs), so machines that run
the harness use `fetch`.

Two checks in the script keep a build usable away from the machine that made it: macOS builds
target macOS 11.0 (`MACOSX_DEPLOYMENT_TARGET`, the first arm64 release) rather than the build
machine's SDK, and a Windows build fails if its DLL imports anything but Windows system DLLs
(MinGW's runtime is on `PATH` on CI runners and in MSYS2, so a load there wouldn't catch it).
To pass it, Windows builds link with `-static` (in `LDFLAGS`), which links MinGW's runtime
into the DLL: the Makefiles' `-static-libgcc -static-libstdc++` alone still leave FBNeo
importing `libwinpthread-1.dll`.

## Forks

A change to a core that the harness needs, such as state a savestate leaves out, is a commit
on a branch of our fork of the core on GitHub (under `zach-youssef`, keeping upstream's
repository name, e.g. `zach-youssef/FBNeo`), and `cores.lock` pins the fork's repository and
that commit. The changes stay in the fork; we don't send them
upstream. Anything else (a build flag, a Makefile quirk) is a `case` in the build script.

- The branch starts at the upstream commit the rule in Pins picks, with one commit per change
  whose message says what was wrong and why it matters to the harness. A core's first branch is `gth` (FBNeo's is; ALE's is `ale-core`). The
  fork's default branch stays upstream's.
- Never rewrite a pinned fork commit or its history (no amend, rebase or force-push of its
  branch), and never delete its branch: releases, recordings and savestates were made from
  it, and rebuilding a release (below) fetches it again. Change a core with new commits on top
  and pin the new head as a bump (Bumping a pin).
- Bumping a forked core to a newer upstream commit takes a new branch from that commit
  (`gth-<upstream short hash>`, as the first branch keeps the plain name), with the changes upstream still lacks cherry-picked onto it,
  so the old branch and the commits pinned before stay as they are.
- Cores that report `git rev-parse --short HEAD` report the fork commit's hash, and FBNeo
  dates its version with that commit's author date: the build at `9ee23cb` reports
  `v1.0.0.03 261127 GIT9ee23cb`.
- Some of FBNeo's source files have CRLF line endings (`src/burn/drv/psikyo/d_psikyo.cpp`
  among them). Edit them with an editor or tool that keeps them (`git apply`, `sed`, Python
  with `newline=''`): Python's text mode rewrites every line, and the commit's diff becomes
  the whole file.
- FBNeo's fork also exposes the named state areas the harness reads labels from
  (docs/libretro.md, "Named areas"): `8ece753` adds the private environment command
  `GTH_ENVIRONMENT_SET_AREA_INTERFACE`, and `9ee23cb` gives the Psikyo driver's sprite-list
  buffers unique area names (`Psikyo sprite lists`, `... index`, `... ends`). The
  libretro port's savestates hold the areas' bytes in scan order, without their names, so
  renaming them changes no savestate: a state saved by the build at `d09bde2` loads in one
  at `9ee23cb` and continues frame-, RAM- and state-identical (checked on Gunbird and
  Strikers 1945).
- ALE's fork (`zach-youssef/Arcade-Learning-Environment`, branch `ale-core`) adds the libretro
  core ALE doesn't have (`libretro/ale_libretro.cpp`; its README describes the core): one
  frame of ALE's `act()` per `retro_run`, the RetroPad's d-pad and B mapped to ALE's 18
  actions (docs/actions.md), no sticky actions, savestates from `cloneSystemState()`, the 128
  bytes of RAM as `RETRO_MEMORY_SYSTEM_RAM` and a synthetic memory-map region at `0x10000`
  with ALE's score, lives, game over and frame counts (layout in the fork's
  `libretro/README.md`). The score is a signed little-endian i32 at offset 0 (Pong, Skiing
  and others go negative), which game configs read with the `sle` RAM encoding (docs/games.md,
  "RAM variables").
  It checks a ROM's MD5 before ALE's `loadROM`, which exits the process for an unknown ROM, so
  a bad ROM fails the load, and refuses another game's or a damaged savestate (its envelope
  holds the cartridge's MD5 and a hash of the header and the state). Its audio is silent (ALE's sound isn't in its
  savestates, so playing it would break state determinism): `gth-play` plays ALE games
  without sound.
  The fork also removes both copies of upstream's bundled ROM, its ROM-fetching scripts and the workflows
  that use them; its own workflow builds the core and runs its unit test, with no ROM, on
  Linux, macOS and Windows. `tests/test_ale_core.py` loads the core (from `data/cores/`, any
  build) and checks that loads of no ROM, a missing file and an unknown one fail cleanly.
- License: ALE is GPL-2.0, and so is its core. The harness (MIT) never links it: like every
  core, it is a separate library loaded at runtime with `dlopen` (`LoadLibrary`), built and
  published as its own binary, and the harness talks to it only through the libretro C API.
- To try a fork commit before pushing it, pass the build script the local clone's absolute
  path as the repository (Builds, above; a relative one would resolve in the build's clone).

## ALE parity

`scripts/ale_parity.py` checks the `ale` core against ale-py itself (the package the published
ALE baselines ran): it plays the same random actions (uniform over ALE's 18, each held 1-8
frames) through ale-py with `repeat_action_probability` 0 and frame skip 1 and through
`RetroEnv` with the game's config at frame skip 1, and compares every frame's RGB SHA-1,
reward, lives and game over, and the episode and load frame counters (so the reset and the
first frame fall on the same emulated frame), across game overs and resets. It needs ale-py
importable next to the harness and uses its bundled ROMs (docs/roms.md, "Atari ROMs"):

```sh
# python3.12: the venv's Python, which must be the harness's (ale-py's wheel is per version)
PYTHONPATH=../alepy/lib/python3.12/site-packages uv run python scripts/ale_parity.py data --frames 30000
```

Later episodes start one of two ways, the protocol's `start.later_episodes`
(docs/evaluation.md). By default the script reloads the ROM before ale-py's `reset_game()`,
which is the harness's load state, against `later_episodes = "reload"`: every episode is then
an ALE first episode. With `--reset-game` ale-py runs `reset_game()` alone, as ALE v5's
environments do (`AtariEnv.reset` reloads the ROM only when given a seed), which doesn't
return the machine to its load state, and the harness runs `later_episodes = "chain"`: the
core's `retro_reset`, which is ALE's `reset_game()` (the fork's `libretro/README.md`), from
where the last episode ended. The load frame counter runs on through a chained reset in both.

On the pinned core (`ef6d7ff`) and ale-py 0.12.1, all twelve Atari games match both ways for
30,000 frames each (4 to 17 episodes), so a mismatch is a shim bug. The two ways differ in
some games from the second episode on (Assault, Demon Attack, Seaquest and Yars' Revenge
before chaining existed, when the harness reloaded under `--reset-game`; the other eight
didn't), which is why the protocol states one. The Atari presets chain, as ALE v5 does.

## The cores workflow

`.github/workflows/cores.yml` runs on a push that changes `cores.lock`, the build script, the
workflow or its setup action, and on manual dispatch:

1. `plan`: `gth cores plan` prints the release tag and each core's source. The tag is
   `cores-` and 16 hex digits of a SHA-256 over every core's name, repository and commit, so
   writing the hashes into the lock keeps it.
2. `build`: every core on macos-latest (arm64), ubuntu-latest and windows-latest, each
   uploaded as the workflow artifact `core-<target>-<name>`.
3. `check`: per platform, `gth cores check` loads each build without a game and checks its
   `library_version`.
4. `stage`: `gth cores release` lays out the builds under their asset names with
   `SHA256SUMS` and the lock with their hashes written in (`cores.lock`), prints
   `SHA256SUMS` into the run log and uploads the lot as the workflow artifact `release`.
5. `publish`, on main only: the one job with write permission. It downloads `release` and
   runs `gh release create` for the tag, with no checkout, harness or third-party action that
   could misuse its token.

On other branches the workflow stops after `stage`, so a branch never publishes anything. A
tag's release is built once: when it already exists, main runs `fetch` on each platform
instead, which checks the hashes committed in `cores.lock` against the release.

After a release, check its `cores.lock` asset's hashes against `SHA256SUMS` in the `stage`
and `publish` logs of the run that built it (an asset can be replaced after publication; the
log can't), then commit it to main, replacing the repo's (only the `artifacts` tables
change): the lock then pins those builds, and `fetch` works. The hashes are trusted on first
use, from that run. The repository's immutable releases setting stops assets from changing
after publication.

## Bumping a pin

1. Pick the commit by the rule above (for a forked core, the head of a new fork branch from it:
   Forks, above) and change its `commit` in `cores.lock`. Clear the
   core's `[cores.<name>.artifacts]` hashes, which are for the old build.
2. Set `library_version` to what the new build reports: build it locally and run `gth cores
   check` (above), or push and read the `check` job's output, which fails and prints the
   reported version until the lock matches.
3. Update the table above, with the reason for the commit.
4. Merge; the workflow on main publishes the new tag's release. Commit its `cores.lock`.

A bump invalidates every savestate and recording made with the old build: a core's savestate
format and its emulation can change between commits, so reset states (`reset.state` in
`game.toml`, checked by SHA-1) and recorded episodes no longer replay. Make new reset states,
update their SHA-1s in the games' configs and re-record what the bump affects, in the same
change. An FBNeo bump can also change the arcade sets' member lists: compare the new
driver tables with the `games/` content tables (docs/roms.md, "Content tables").

Changing how a core is built (the script, or the runner images) without changing a pin keeps
the tag, so main won't rebuild it. To publish new builds anyway, delete the tag's release
(`gh release delete <tag> --cleanup-tag`), dispatch the workflow on main and commit the new
release's `cores.lock`. That also invalidates savestates and recordings when the builds
differ.

## Adding a core

Add a `[cores.<name>]` entry (repository, commit, `library_version`, empty `artifacts`), a
`case` for its build in `scripts/build-core.sh` and a row in the table above, then release
it as for a bump. `<name>` is the libretro name the library is built as
(`<name>_libretro.<ext>`) and what `game.toml`'s `core` names.
