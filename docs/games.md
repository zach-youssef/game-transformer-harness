# Game configs

Each game the harness runs has `games/<game-id>/game.toml`, parsed and validated by
`gth_config::GameConfig` (`crates/gth-config/src/game.rs`). A config that parses is one the
harness can use: unknown fields, bad values and conditions on missing RAM variables are
errors that name the field. `games/testcore/game.toml` (gth-testcore as a game) uses every
field, and `RetroEnv.from_game` (architecture.md, "Observations and RAM") builds an
environment from a config.

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
members are checked by the ROM audit (P2), not by the lookup here:

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

A game under discovery (RAM search, P4) can use a stub config: no `[ram]`, `terminate = "0"`
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

## `cores.lock`

The pinned build of each core, parsed by `gth_config::CoresLock` and filled by `cores fetch`
(P1):

```toml
[cores.fbneo]
repo = "https://github.com/libretro/FBNeo"
commit = "<40 hex digits>"
library_version = "<what retro_get_system_info reports>"

[cores.fbneo.artifacts]   # SHA-256 of the built library per target triple
aarch64-apple-darwin = "<64 hex digits>"
```
