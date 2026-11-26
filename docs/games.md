# Game configs

Each game the harness runs has `games/<game-id>/game.toml`, parsed and validated by
`gth_config::GameConfig` (`crates/gth-config/src/game.rs`). A config that parses is one the
harness can use: unknown fields, bad values and conditions on missing RAM variables are
errors that name the field. `games/testcore/game.toml` (gth-testcore as a game) uses every
table and RAM variable key (the other forms of `reset` and `content`, a savestate and
`in_parent`, are tested with configs the tests write), and `RetroEnv.from_game`
(architecture.md, "Observations and RAM") builds an environment from a config.

## Adding a game

1. **Pick the id**: the directory name under `games/`, lowercase with hyphens
   (`lightening-force`). Recordings, savestate paths (`<data_dir>/states/<id>/`) and the
   audit name the game by it, so don't rename it later.
2. **Content and BIOS hashes** (below, "Content and BIOS files"): size, SHA-1 and CRC-32 in
   lowercase hex, from the user's own dump, never from a download (docs/roms.md, "Rules").
   On the Mac `~/roms/manifest.md` lists names, sizes and SHA-1s; `shasum -a 1 <file>` and
   Python's `zlib.crc32` give the rest. `gth audit` also prints them: a file named as in
   `content.file` at the top of the ROM directory is reported `bad-hash` with the size and
   hashes found, and for an archive `--json` lists the members that match no expected
   member, with their hashes.
3. **Audit**: `uv run gth audit <data_dir> --game <id>` (docs/roms.md, "Auditing") must report
   the game `ok` on a machine that has its files: content found by SHA-1, parents beside it,
   BIOS files under `<data_dir>/system`, archive members by hash. Add the game to the content
   table in docs/roms.md, saying where its hashes came from.
4. **Core, frame, buttons and reset**: `core` is the core's name in `cores.lock`
   (docs/cores.md, coming with P1) and, now and after, the stem of its library's file name,
   `<core>_libretro.<so|dylib|dll>` (docs/roms.md, "Data directory"), so use the libretro
   build's real name (`mednafen_pce_fast`); `orientation` and `crop` give the picture a policy sees;
   `[buttons]` maps the game's controls onto the four slots (docs/actions.md); `[reset]` gets
   from power-on to the first controllable frame with a script, or names a savestate.
5. **RAM variables and the episode end** (below). Each is validated in three stages:
   - parsing rejects a bad name, encoding or size, a `scale` that can overflow, and a
     `terminate` condition naming a variable that doesn't exist;
   - making a `RetroEnv` checks every variable's bytes against the core's memory and memory
     map, so a memory the core doesn't expose, or bytes outside it or outside one map region,
     fail at once (`HarnessError` naming the variable); a wrong offset that is still in range
     doesn't;
   - neither can tell whether the bytes mean what the name says, so check the values
     against the game: step a `RetroEnv` from reset and compare `info["ram"]` with the screen
     at known points (the score after a kill, the lives after a death, the stage after a
     clear), from the end of the reset script on, where a BCD variable must already hold
     valid digits. Pin those checks in a ROM test (docs/testing.md, "ROM tests"), which
     skips on CI. Finding the addresses in the first place is RAM search (below).
6. **Test**: at least a ROM test that makes the game's `RetroEnv` from reset and checks the
   RAM variables and episode end as above. Besides `rom` it needs the real core and the data
   directory: the first such test adds the fixture that finds them and skips with a reason
   when either is missing (docs/testing.md, "ROM tests").

A game can land in stages: until its RAM variables are found, a stub with only `[content]`
final (one button, a one-frame reset script, no `[ram]`, `terminate = "0"`) parses and lets
the audit check the content, as `games/gunhed` does. Say in the file's header comment what is
still a stub.

### Changing the schema

A new `game.toml` field goes in `GameConfig` (`crates/gth-config/src/game.rs`), the table
below, `games/testcore/game.toml` (`test_testcore_config_uses_every_field` checks that it uses
every table and RAM variable key; a variant it can't hold, such as `reset.state`, gets a test
that writes its own config, as in `tests/test_games.py`) and the `_native.GameConfig` getters
`RetroEnv` needs (docs/native.md). Recordings
store their `game.toml`, so a change old files no longer parse under is a recording format
change (docs/recording-format.md, "Versioning").

## Fields

| Field | Required | Meaning |
|---|---|---|
| `core` | yes | The core's name in `cores.lock` (below). |
| `orientation` | yes | Clockwise degrees to turn the frame: 0, or 90 / 270 for vertical (tate) games. |
| `crop` | no | `[x, y, width, height]` of the core's frame to keep, before rotation, in core pixels. Omitted: the whole frame. |
| `[content]` | yes | The file the core loads, its `parents` and `in_parent` (below). |
| `[[bios]]` | no | Files the core needs in the system directory, same form as `content`; `file` may be a relative path (`fbneo/pgm.zip`). |
| `[buttons]` | yes | RetroPad button -> action slot (docs/actions.md). |
| `[reset]` | yes | How an episode starts (below). |
| `[ram.<name>]` | no | Named RAM variables (below). |
| `[episode]` | yes | `terminate`: when an episode ends (below). |
| `[core_options]` | no | `key = "value"` core options (`SET_VARIABLES` keys). Others keep the core's defaults. |

### Content and BIOS files

```toml
[content]
file = "gunhed.pce"       # the name the core expects
size = 393216             # bytes
sha1 = "..."              # 40 lowercase hex digits
crc32 = "..."             # 8 lowercase hex digits
```

A plain file is identified by size and hashes, whatever its name (docs/roms.md). An archive
whose own hash depends on how it was packed (an arcade set's zip) gives `members` instead of
`size`/`sha1`/`crc32`, one inline table per file inside it, and is found by name; its
members are checked by the ROM audit (docs/roms.md, "Auditing"), not by the lookup here:

```toml
[content]
file = "ddpdojblk.zip"
members = [
  { name = "prg0.bin", size = 524288, sha1 = "...", crc32 = "..." },
]
parents = [               # the clone's parent set, which the core loads from beside it
  { file = "ddp3.zip", members = [ ... ] },
]
```

`parents` (content only) lists files the core needs next to the content, such as an arcade
clone's parent set; each has the same form as `content`, without `parents`. They are
checked in the directory where the content was found.

That is a split set, where the clone has a zip of its own. In a merged set the parent's zip
holds its clones and no clone zip exists; say so with `in_parent = true` (content only, an
archive with `parents`):

```toml
[content]
file = "ddpdojblk.zip"    # no such file: the clone is inside ddp3.zip
in_parent = true
members = [ ... ]
parents = [{ file = "ddp3.zip", members = [ ... ] }]
```

The harness then finds the first parent and gives the core `<its directory>/ddpdojblk.zip`, a
path that doesn't exist. A core that loads content from its path (`need_fullpath`) takes the
set's name from it and finds the ROMs in the parent's zip: FBNeo tries the clone's zip, then
the parent's. A core that reads content into memory can't, and `RetroEnv` raises
`ConfigError`. A non-merged set is a single zip holding everything, with no `parents`.

The content and its parents have plain file names. A BIOS `file` may be a relative path
under the system directory, for cores that look in a subdirectory: FBNeo reads its BIOS sets
from `system/fbneo/`, so DOJ's PGM BIOS is `file = "fbneo/pgm.zip"`. Paths use `/`, and no
component may be `.`, `..` or contain `\` or `:`, so a path can't leave the system
directory.

### Buttons

Keys are RetroPad buttons by their `RETRO_DEVICE_ID_JOYPAD_*` names in lowercase (`b`, `y`,
`a`, `x`, `l`, `r`, `l2`, `r2`, `l3`, `r3`); values are slots: `shot`, `bomb`, `aux1`, `aux2`.
The d-pad isn't listed (the action's direction drives it), and `select` and `start` are
rejected: coin and start are pressed only by reset scripts, never by a policy. Two buttons
may share a slot, which then presses both.

### Reset

The core first goes back to its state right after loading (docs/libretro.md, "Host"), then
one of:

```toml
[reset]
script = [
  { press = ["select"], frames = 4 },    # coin
  { press = [], frames = 30 },
  { press = ["start"], frames = 4 },
  { press = [], frames = 240 },          # wait for the stage
]
```

```toml
[reset]
state = { file = "stage1.state", sha1 = "..." }   # in <data_dir>/states/<game-id>/
```

A script holds each step's buttons (any RetroPad button) for its frames. A savestate is made
from the user's ROM, so it lives in the git-ignored data directory, never in `games/`; its
SHA-1 is in `game.toml`, and `RetroEnv` refuses a file that doesn't match (`ConfigError`).
A savestate only fits the core build that made it, so record it again, with its new hash,
when the core in `cores.lock` changes. Either way the environment then runs `frameskip`
no-op frames for the first observation.

### RAM variables

```toml
[ram.score]
memory = "system_ram"     # system_ram | save_ram | video_ram | rtc
offset = 0x10
size = 4                  # bytes, 1-8
encoding = "bcd"          # bcd | le | be
swap16 = false            # optional, default false
scale = 10                # optional, default 1

[ram.lives]
address = 0xF80012        # an address in the core's memory map, instead of memory + offset
size = 1
encoding = "le"
```

- `bcd` is packed BCD, two digits per byte, most significant byte first. `le` and `be` are
  unsigned.
- `swap16` swaps the two bytes of each 16-bit word before decoding: FBNeo keeps 68k RAM as
  host-endian 16-bit words, so a value's bytes come out swapped in pairs.
- The value is multiplied by `scale` (a score stored in tens has `scale = 10`). A variable
  whose largest value times `scale` doesn't fit an `i64` (8 `le` bytes, say) is rejected
  when the config is parsed.
- Names are identifiers. Use `score`, `lives`, `bombs`, `stage` and `credits` for those
  meanings, so rewards and tools work across games; add others as needed.

RAM variables are for rewards, episode ends and logging, never for the policy: game RAM is
never part of an observation (architecture.md, "Observations and RAM"). `RetroEnv` passes
them, by name, to its reward function and in `info["ram"]`.

When `RetroEnv` is made, it checks every variable against the core's memory: the memory
exists, the bytes are inside it, an address lies in one memory map region. A config that
fails is rejected then (`HarnessError` naming the variable), not mid-episode. After that a
read can fail only on a `bcd` variable holding a byte that isn't BCD, and then `step()` or
`reset()` raises `HarnessError` naming the variable and the byte. That ends a training run,
so pick BCD variables (and reset script end points) where the game always keeps them valid:
some games write garbage to the score during boot or attract screens.

A game under discovery ("RAM search", below) can use a stub config: no `[ram]`, `terminate = "0"`
and a reward function that doesn't read RAM.

### Episode

```toml
[episode]
terminate = "lives == 0 && game_over != 0"
```

A boolean expression over RAM variables, checked after every step: integers, variable
names, `== != < <= > >=`, `&& || !`, unary `-` and parentheses; a value is true when it
isn't 0. `||` binds loosest, then `&&`, then the comparisons, as in C, but a comparison
can't take another as an operand (`a < b == c` is an error: parenthesize). Every name must
be a `[ram]` variable. A condition has at most 256 tokens and nests unary operators and
parentheses at most 32 deep.

## RAM search

RAM search finds a game's RAM variables and checks them, the way an emulator's cheat search
does: snapshot a memory, play a little, keep only the candidates whose value changed as
expected, repeat until few are left. It is a developer tool for writing `[ram]` tables;
game RAM is never a policy's input (architecture.md, "Observations and RAM").

A search covers one **space**: a core memory (`system_ram`, `save_ram`, `video_ram`, `rtc`),
by offset, or `memory_map`, every memory map region that isn't constant (ROM), by emulated
address; a mirror (the same memory mapped at several addresses) is searched once, at its
lowest address. Addresses read as `[ram]` variables do, through the first region registered
that holds them, so if a core registered other memory over that lowest address first, the
search reads that memory there and can miss a variable held only in the mirror (no known core
does this). It reads each candidate through one or more **views**, all searched at once:

| View | Reads |
|---|---|
| `le1`, `bcd1` | one byte, unsigned or packed BCD |
| `le2`, `be2`, `bcd2`, `bcd2-swap16` | two bytes |
| `le4`, `be4`, `bcd4`, `le4-swap16`, `be4-swap16`, `bcd4-swap16` | four bytes |

The encodings are those of RAM variables (above, "RAM variables"); `-swap16` swaps the bytes
of each 16-bit word first (FBNeo's 68k RAM) and is searched only at even offsets (even
addresses in `memory_map`). `be1` and `le2-swap16` are accepted but repeat `le1` and `be2`;
`all` names the twelve above. A BCD candidate is dropped whenever its bytes aren't BCD.

A **relation** compares each candidate's value with its value at the previous snapshot:
`equal`, `changed`, `increased`, `decreased`, or `equals` a given value (the number on
screen). Every filter takes a new snapshot, so the next one compares with it.

### Scripts

A search script (`gth_config::SearchScript`) plays a game from its reset and narrows the
candidates step by step, so a search can be rerun after a core update or kept beside the
game as the evidence for its `[ram]` table:

```toml
space = "system_ram"
views = ["all"]           # or a list: ["bcd4", "le2"]

[[step]]                  # hold shot for 300 frames: the score went up
press = ["a"]
frames = 300
expect = "increased"

[[step]]                  # wait without shooting: it stayed
frames = 120
expect = "equal"

[[step]]                  # the score on screen now
expect = "equals"
value = 1500
```

Each step, in order: loads `state`, a savestate file relative to the script's directory, if
given; holds `press` (RetroPad buttons, as in a reset script) for `frames` frames; then
filters by `expect` (and `value`, for `equals` only). A step without `expect` takes a new
snapshot without comparing (it drops only BCD candidates that stopped being BCD). For
lives, use pairs of savestates made just before and just after a death: a `state` step with
the first, then one with the second and `expect = "decreased"`, once per pair.

### Running a search

`gth ram-search <game> <data_dir> <script.toml>` loads the game as `RetroEnv.from_game` does
(`--rom-dir`, `--core-path` and `--games-dir` as there), resets it, runs the script and prints
the candidate counts (at the start, then after each step) and the first `--limit` (20)
candidates, each as a `[ram]` table to paste and rename. `--watch FRAMES` then runs that many
frames (at most 108000, and 10 million values in all) with no button held and prints the
candidates' values after each, which tells a score apart from a timer; `--json` prints it
all as JSON. Exit status: 0 when candidates are left, 1 when none are, 2 on an error (a bad
script, a missing savestate, a watch over its caps).

From Python, `gtharness.ramsearch.run_script(env, path)` runs a script on an environment and
returns the `_native.RamSearch` (docs/native.md), which can keep filtering by hand (`filter`,
`rebase`), list `candidates(limit)` and `watch` them over given inputs. The search runs in
Rust; over a large memory map, build the extension with `maturin develop --release`.

### Checking variables in tests

`gtharness.ramsearch.assert_ram(info["ram"], name, expected)` asserts that a RAM variable
equals a value, or satisfies a predicate, after known play: a game test plays a script from
the reset (or a savestate) and checks the score or lives it should leave. Tests that need the
game's ROM skip without it (docs/testing.md).

## `cores.lock`

The pinned build of each core, parsed by `gth_config::CoresLock`. The schema is ready, but
nothing reads a lock yet: there is no `cores.lock` file, `RetroEnv` takes the core from
`core_path` or `<data_dir>/cores/` (docs/roms.md, "Data directory"), and recordings carry no
`[core.lock]`. P1 adds the file, `cores fetch` and the wiring (docs/cores.md, coming with P1):

```toml
[cores.fbneo]
repo = "https://github.com/libretro/FBNeo"
commit = "<40 hex digits>"
library_version = "<what retro_get_system_info reports>"

[cores.fbneo.artifacts]   # SHA-256 of the built library per target triple
aarch64-apple-darwin = "<64 hex digits>"
```
