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
   member, with their hashes. An arcade set's `members` are the exception: their names,
   sizes and CRC-32s come from the pinned FBNeo's driver table and their SHA-1s from MAME's
   driver where the CRC-32s agree (docs/roms.md, "Content tables"), and the audit against
   the user's zip is the check.
3. **Audit**: `uv run gth audit <data_dir> --game <id>` (docs/roms.md, "Auditing") must report
   the game `ok` on a machine that has its files: content found by SHA-1, parents beside it,
   BIOS files under `<data_dir>/system`, archive members by hash. Add the game to the content
   table in docs/roms.md, saying where its hashes came from.
4. **Core, frame, buttons and reset**: `core` is the core's name in `cores.lock`
   (docs/cores.md, "Adding a core") and the stem of its library's file name,
   `<core>_libretro.<so|dylib|dll>` (docs/roms.md, "Data directory"), so use the libretro
   build's real name (`mednafen_pce_fast`); `settle_frames` (below, "Settle frames") is 0
   unless the core keeps state outside its savestates; `orientation` and `crop` give the picture a policy sees;
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
6. **Evidence**: commit each variable's search script in `games/<id>/searches/<variable>.toml`,
   and write `games/<id>/README.md`: where each part of the config came from (buttons, reset,
   each address and its source, what its values mean), scoring notes, and how to make any
   savestate a script needs (savestates are never committed: `*.state` is git-ignored).
7. **Test**: `tests/test_<id>.py`, ROM tests taking the `rom` and `core` fixtures, so they skip
   with a reason without the ROM or the pinned core (docs/testing.md, "ROM tests"): a scripted
   session from the reset, recorded, that checks with `assert_ram` that the score rises, the
   lives fall and the episode ends where it should, then replays the recording to the end
   (a desync raises); and one test per search script that it still narrows to the config's
   address.

A game can land in stages: until its RAM variables are found, a stub with only `[content]`
final (one button, a one-frame reset script, no `[ram]`, `terminate = "0"`) parses and lets
the audit check the content (`games/zanac` did until its config was finished). Say in the
file's header comment what is still a stub.

### Example: Gunhed

`games/gunhed` is the worked example; copy its layout for a new game.

- **Content** (steps 1-3): the id `gunhed`, the hashes from `~/roms/manifest.md`, no BIOS.
- **Buttons**: which RetroPad button reaches which console button is the core's mapping
  (`mednafen_pce_fast`: B is II, A is I), and which one fires is the game's, so try each with
  a scripted `RetroEnv` and look at the frames. Note controls the slots can't reach (Gunhed's
  speed change is on Select) in the README.
- **Reset**: find the earliest frames that accept Start by stepping the core from power-on
  with Start held over different windows and saving frames; then find where the d-pad and a
  button first change the picture (two runs from the same savestate, one pressing, one not).
  End the script there (`RetroEnv.reset` then runs `frameskip` more frames before the
  first action), so a policy's first action counts.
- **RAM**: confirm the published addresses and find the rest with one script each, against
  the HUD, from the reset where possible (`score.toml`, `lives.toml`, `bombs.toml`,
  `mode.toml`), else from savestates (`stage.toml`). Each must end with the variable
  alone (the BCD and plain views of the same byte count as one); "RAM search" below has the
  tricks that get there.
- **Episode end**: check `terminate` across a stage clear, not only across deaths. Gunhed's
  byte 0x375 looked like a game-over flag in every run that only died, but it is a mode
  byte that is also 0x80 during each area clear, so `!= 0` would have ended every episode
  at the first boss; `terminate = "mode == 64"` names the GAME OVER value.
- **Test**: `tests/test_gunhed.py` shoots for 150 steps from the reset (the score reaches
  1300), then flies up into the enemies until GAME OVER, pinning the steps where ships are
  lost, and replays the recording; from a savestate before the Area 1 clear it plays into
  Area 2 and checks the episode goes on. The pinned numbers hold on the pinned core.

### Example: Zanac (NES)

`games/zanac` is the worked example for an NES game on `fceumm`:

- **Header and core**: the Project Egg dump's iNES header (mapper 2, CHR-RAM) loads as it is,
  so there is no core option or override; a dump that misloads is fixed in `game.toml`
  (`core_options`), never in the ROM file.
- **Memory**: the work RAM is `system_ram`, 2 KB (CPU $0000-$07FF); sprites are drawn from
  a copy of the OAM at $0200, which a search can use to find what is on screen.
- **Buttons**: RetroPad B and A are the NES B and A.
- **Reset**: the title takes two Start presses (menu, then GAME START); end the script where
  the ship first answers the d-pad.
- **RAM**: the score is one digit per byte (`digits`), which no search view reads: find it
  by dumping the RAM at known scores. The HUD shows no weapon state, so those variables
  have no number to search against (games/zanac/README.md says what is missing).
- **Episode end**: `terminate = "lives == 0"`, where `lives` counts the ship in play; the
  play flag also drops at every area clear and under every banner, so it isn't an end.

### Atari (ALE core)

`games/atari-<rom>` (ALE's ROM name with hyphens: `atari-ms-pacman`; the prefix keeps an
arcade game of the same name, such as Phoenix, its own id) runs an Atari 2600 game on the
`ale` core (docs/cores.md, "Forks"), so its frames, rewards, lives and game over are ALE's
own. Every Atari config is the same apart from its ROM and minimal action set; the twelve so
far are the pilot set of the Atari presets (docs/evaluation.md):

- **Content**: the ROM file ale-py 0.12.1 bundles (docs/roms.md, "Atari ROMs"), with its
  size, SHA-1 and CRC-32, and `md5` from ALE's game table (the fork's
  `src/ale/games/supported/<Game>.hpp`), which the core checks before loading and `gth
  audit` prints beside the file's.
- **Buttons**: `b = "shot"`, the joystick's fire button; the d-pad is the joystick. ALE's 18
  actions are the direction and `shot` (docs/actions.md, "Atari").
- **Minimal action set**: `atari_minimal_actions`, ALE's `getMinimalActionSet` for the ROM
  in its default mode and difficulty, for the `atari_minimal` action set.
- **Reset**: `script = []`. The core's load runs ALE's `reset_game()` (its own start-up
  no-ops and starting actions, the mode and the difficulty), so the load state is ALE's
  first frame of an episode, and `RetroEnv.reset` goes back to it.
- **RAM**: `score`, `lives` and `game_over` come from the core's synthetic region at
  `0x10000`, not from the game's RAM: `score` (`sle`, offset 0) is the sum of ALE's rewards
  since the reset, so its per-step change is ALE's reward; `lives` (offset 8) is ALE's
  `lives()`; `game_over` (offset 12) is 1 once ALE's `game_over()`. They feed rewards and
  the episode end only, never a policy.
- **Episode end**: `terminate = "game_over != 0"`, so a lost life isn't terminal (the
  presets' `life_loss_terminal = false`). The core has no frame cap of its own: ALE v5's
  108,000-frame cap is the protocol's `max_episode_frames`.
- **Options**: `ale_mode` and `ale_difficulty` are `"default"`, the game's own, as in ALE
  v5's environments. ALE's sticky actions are the env's `sticky_prob` (docs/actions.md), and
  the core plays none of its own.
- **Tests**: `tests/test_ale.py` checks every config against this recipe and, with the ROMs
  and the pinned core, that episodes are deterministic, a sticky-action recording replays,
  the score is ALE's reward sum (Pong's points in its own RAM), a lost life isn't terminal,
  the 108,000-frame cap truncates and `ale_gray` is ALE's grayscale palette exactly.

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
| `settle_frames` | yes | Frames the core runs after each savestate load before loading it again (below, "Settle frames"): 0 for most cores. |
| `orientation` | yes | Clockwise degrees to turn the frame: 0, or 90 / 270 for vertical (tate) games. |
| `crop` | no | `[x, y, width, height]` of the core's frame to keep, before rotation, in core pixels. Omitted: the whole frame. |
| `[content]` | yes | The file the core loads, its `parents` and `in_parent` (below). |
| `[[bios]]` | no | Files the core needs in the system directory, same form as `content`; `file` may be a relative path (`fbneo/pgm.zip`). |
| `[buttons]` | yes | RetroPad button -> action slot (docs/actions.md). |
| `[reset]` | yes | How an episode starts (below). |
| `[ram.<name>]` | no | Named RAM variables (below). |
| `[episode]` | yes | `terminate`: when an episode ends (below). |
| `sprite_hw` | no | The board whose sprite list labels shards (docs/recording-format.md, "Sprite labels"): `psikyo` (Psikyo's 68EC020 boards on our FBNeo fork), or `testcore` (tests). Omitted: the game has no sprite labels, and `--sprites yes` refuses it. |
| `atari_minimal_actions` | no | An Atari game's minimal action set: the ALE action numbers (0-17, docs/actions.md "Atari"), ascending, that ALE's `getMinimalActionSet` gives for the ROM. A protocol's `action_set = "atari_minimal"` needs it (docs/evaluation.md). Omitted: not an Atari game. |
| `[core_options]` | no | `key = "value"` core options (`SET_VARIABLES` keys). Others keep the core's defaults. |

### Settle frames

Some cores keep state they derive while running outside their savestates, so the first frame
after a load shows what the process ran before it, and a recording doesn't replay in a fresh
process (its first frame hash differs while RAM matches). FBNeo's Toaplan GP9001 drivers
(`bgaregga`) draw each frame with the palette computed after the previous one, which is
outside the state, and nothing recomputes it on load (docs/performance.md, "FBNeo and
savestates"). With
`settle_frames = n`, every load (`Core::unserialize`, `Core::reset`, the replayer's and the
recorder's start, `RetroEnv.reset`) runs `n` frames with no button held, unseen, and loads
the state again, so the next frame depends only on the state. It isn't quite the frame the
original run showed: that one used the palette from the frame before the state, which the
state doesn't hold, so on a frame where the palette changes the two differ. The frames after
it are exact, which is why gth-play's rewind seek (docs/human-play.md) replays 1 +
`settle_frames` frames past a savestate. Recordings and replays settle alike, so recordings
replay with no desync (for the FBNeo games together with our FBNeo fork's commits,
docs/cores.md, "Forks", which add state whose loss the frames after a load don't recover
from).

Set it to the fewest frames that make the first frame after a load the same whatever ran
before (1 for the FBNeo games above), checked on the game's ROM as `tests/test_bgaregga.py`,
`tests/test_psikyo.py` and `tests/test_pgm.py` do; 0 loads states as they are, the right value for a core whose
savestates hold everything. A game's value is stored with its recordings (in their
`game.toml`), and a replay refuses a core loaded with another one. Psikyo games (`gunbird`,
`s1945`) hold everything in their states on the pinned build (docs/performance.md, "FBNeo and
savestates"), but keep `settle_frames = 1` too: FBNeo's libretro port dupes the first frame a
process runs, so an unsettled load in a fresh process shows a cleared frame first. The IGS PGM
games (`ket`, `ddp3`, `ddpdojblk`) likewise hold everything in their states on the pinned
build (the PGM driver rebuilds its palette from the loaded palette RAM) and keep 1 for the
same dupe (`tests/test_pgm.py`).

### Content and BIOS files

```toml
[content]
file = "gunhed.pce"       # the name the core expects
size = 393216             # bytes
sha1 = "..."              # 40 lowercase hex digits
crc32 = "..."             # 8 lowercase hex digits
md5 = "..."               # optional, 32 lowercase hex digits
```

A plain file is identified by size and hashes, whatever its name (docs/roms.md). `md5` is
for a core that identifies its content by MD5 (the ALE core looks ROMs up in ALE's game
table by it): the audit checks it and prints it beside the file's, but the lookup doesn't
need it. An archive
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

A script holds each step's buttons (any RetroPad button) for its frames. An empty script
(`script = []`) starts each episode at the load state itself, for a core whose load already
starts the game: the ALE core's load runs ALE's own game reset ("Atari (ALE core)"). A savestate is made
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
encoding = "bcd"          # bcd | le | be | sle | digits
swap16 = false            # optional, default false
scale = 10                # optional, default 1

[ram.lives]
address = 0xF80012        # an address in the core's memory map, instead of memory + offset
size = 1
encoding = "le"
```

- `bcd` is packed BCD, two digits per byte, most significant byte first. `le` and `be` are
  unsigned. `sle` is signed (two's complement) little-endian, the sign being the top bit of
  the `size` bytes: the ALE core's score, an i32 that goes negative in Pong, Skiing or Boxing
  (docs/cores.md, "Forks"). RAM search has no signed view; its `le` views find a signed
  variable's non-negative values. `digits` is one decimal digit (0 to 9) per byte, most significant first: a score
  a game keeps as the digits it draws (Zanac's, six bytes). Like `bcd`, a byte above 9 makes
  a read fail. RAM search has no view for it (below), so its bytes are found by looking at
  the RAM: dump the work RAM at known scores and look for the digits.
- `swap16` swaps the two bytes of each 16-bit word before decoding: FBNeo keeps 68k RAM as
  host-endian 16-bit words, so a value's bytes come out swapped in pairs.
- The value is multiplied by `scale` (a score stored in tens has `scale = 10`). A variable
  whose largest value times `scale` doesn't fit an `i64` (8 `le` bytes, say) is rejected
  when the config is parsed; for `sle` that is its most negative value (8 `sle` bytes fit
  with `scale = 1` only).
- Names are identifiers. Use `score`, `lives`, `bombs`, `stage` and `credits` for those
  meanings, so rewards and tools work across games; add others as needed. `gth-play` rewinds
  when `lives` goes down (docs/human-play.md, "Rewind"), so it must fall on every lost life.

RAM variables are for rewards, episode ends and logging, never for the policy: game RAM is
never part of an observation (architecture.md, "Observations and RAM"). `RetroEnv` passes
them, by name, to its reward function and in `info["ram"]`.

When `RetroEnv` is made, it checks every variable against the core's memory: the memory
exists, the bytes are inside it, an address lies in one memory map region. A config that
fails is rejected then (`HarnessError` naming the variable), not mid-episode. After that a
read can fail only on a `bcd` variable holding a byte that isn't valid BCD (or, for `digits`,
a byte above 9), and then `step()` or `reset()` raises `HarnessError` naming the variable and
the byte. That ends a training run, so pick BCD variables (and reset script end points)
where the game always keeps them valid: some games write garbage to the score during boot or attract screens.

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

What narrows a search fastest (from `games/gunhed/searches/`):

- Put `expect` on the step that presses: a step without one rebases, so a change it causes
  (a bomb spent as the button goes down) is lost from the next comparison.
- `equals` with the value on screen beats `changed`: sprite and timer bytes also change at
  the moment a flag does, but rarely hold the same value at two such moments
  (Gunhed's `mode.toml` reaches its byte only at the second GAME OVER).
- A value with more digits than a smaller view can hold rules that view out: the score
  past 9999 drops the 2-byte BCD view of its low bytes.
- A game that plays on without input (enemies kill the ship) gives lives and game-over
  scripts from the reset with no savestates.

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

The pinned build of each core, parsed by `gth_config::CoresLock`. The cores workflow builds
and releases it, and `gth cores fetch` checks this platform's build against it (docs/cores.md):

```toml
[cores.fbneo]
repo = "https://github.com/libretro/FBNeo"
commit = "<40 hex digits>"
library_version = "<what retro_get_system_info reports>"

[cores.fbneo.artifacts]   # SHA-256 of the built library per target triple
aarch64-apple-darwin = "<64 hex digits>"
```
