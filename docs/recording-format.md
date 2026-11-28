# Recording format

A recording is a replay (decision U4): the savestate it starts from plus the RetroPad mask of
every emulated frame, with periodic hashes to detect a replay that drifts. Frames aren't
stored; they are re-rendered by replaying, at whatever observation size, crop and rotation
the consumer wants. That makes recordings tiny and exact, and ties each one to the core
build that made it (the pinned cores in `cores.lock`): a different build may emulate
differently, and the replay then desyncs. `gth-record` (`crates/gth-record`) writes, reads
and replays them.

## Layout

A recording is a directory `<id>.gthrec/`:

| File | Holds |
|---|---|
| `meta.toml` | What and how (below). Written last: a directory without it is an incomplete recording, which readers refuse. |
| `game.toml` | The game config the recording was made with (canonical `GameConfig::to_toml`), so the button map, core options and RAM variables of the time stay with it when `games/` changes. |
| `start.state` | The core's savestate at the first frame, zstd. |
| `inputs.bin` | One record per emulated frame, zstd. |
| `checks.bin` | State hashes and RAM values every `checks_every` frames, zstd. |

Integers are little-endian. Every file is checked when a recording is opened: `game.toml`
and `start.state` against their SHA-1s in `meta.toml`, the frame and step counts of
`inputs.bin`, and the frames of the checks.

### meta.toml

| Field | Meaning |
|---|---|
| `format_version` | `gth_record::FORMAT_VERSION` (2). Readers read it and older versions back to `OLDEST_FORMAT_VERSION` (1) through migrations (below, "Versioning") and refuse others, checked before the other fields. |
| `game_id` | The game's directory under `games/`. |
| `game_config_sha1` | SHA-1 of `game.toml`. |
| `recorder` | `human` (gth-play), `policy`, `scripted` (tests, random or scripted rollouts) or `converted` (from a MAME `.inp`). |
| `player` | A person's handle, the policy and checkpoint that played, or the script, its version and its seed ("Scripted captures"). |
| `license` | The license the recording is shared under. The user's own play is shareable non-commercially (D8), e.g. `CC-BY-NC-4.0`. |
| `created_unix` | Seconds since the Unix epoch when recording started. |
| `fps` | The core's frame rate (`retro_get_system_av_info`). |
| `frames`, `steps` | Emulated frames and agent steps in `inputs.bin`. |
| `checks_every` | Frames between two checks. |
| `[core]` | `name` (game.toml's `core`), `library_name` and `library_version` (`retro_get_system_info`; a replay refuses a core that reports others), and `lock`, the core's entry from the lock the recorder passes to `start_recording` (any lock a direct caller chooses; `RetroEnv` passes the checkout's lock and `gth-play` its `--lock` only when the core file is the build it pins, by SHA-256, docs/cores.md), absent without one (kept as a plain table that replays don't read, so it survives changes to the lock's schema). |
| `[content]` | `file` and `sha1` of what identifies the content the core loaded: the content file, or for a clone `in_parent` (docs/games.md), whose path doesn't exist, the parent archive holding it. A replay refuses content with another SHA-1, no content when the recording has some, and content when it has none. Absent when the core loaded none. Not checked: an `in_parent` clone's own files (the archive's SHA-1 covers them, with every other set in it), the other `parents`, and BIOS files, whose hashes are in the stored `game.toml` for the ROM audit (P2). |
| `[start]` | `sha1` and `size` of the uncompressed start state. |
| `[harness]` | The harness build, `_native.build_info()`: version, git commit and dirty flag, target, profile, rustc. |
| `[branch]` | Only in a side recording branched off another (gth-play's rewind clips, docs/human-play.md): `parent`, the parent recording's directory name (beside this one); `frame`, the parent's frames before this recording's start (its start state is the parent's state after them, on the parent's timeline when the branch was made); `cause`, `lives` (the game's `lives` variable dropped: a death) or `manual` (the rewind hotkey). |
| `[[rewind]]` | One per rewind spliced into this recording that is still on its timeline, in order: `frame`, the frames kept (the frames after them were replaced by what was played after the rewind); `cause`, as in `[branch]`; `clip`, the side recording holding the replaced frames, whose `[branch]` points back here. A later rewind to an earlier frame drops the entries past it: their clips stay, labelled with a `frame` on a timeline the parent no longer holds. Absent when nothing was rewound. |

The recorder, player, license and check interval are required wherever a recording is made
(`RecordingInfo`, `gtharness.record.Record`), so none of them is ever a silent default.

### inputs.bin

The magic `GTHINPUT`, then 3 bytes per frame: the port 0 RetroPad mask as a `u16` (bit n =
`RETRO_DEVICE_ID_JOYPAD_*` n, the input the core saw, d-pad and reset-script buttons
included), and a flags byte whose bit 0 marks the last frame of an agent step (the frame its
observation shows); the other bits must be 0. The last frame always ends a step.

The mask, not the action, is the canonical input: it is what the core consumed, so replay is
exact whatever action space produced it. Under sticky actions it is the effective mask, the
previous frame's when the frame stuck, so a sticky episode replays without the env's RNG. The
flags keep the agent's steps: frameskip is the distance between step ends, so frame-rate and
model-rate data both come from one recording. Unified-slot labels for training are derived from
the mask with the recording's own `game.toml` (below).

### checks.bin

The magic `GTHCHECK`, a `u32` count of RAM variables (those of `game.toml`, in name order),
then one record per check: the frame (`u64`, frames run since the start state, so the first
frame is 1), a frame hash and a RAM hash (`u64` each: the first 8 bytes of a SHA-1, of the
frame's size, pixel format and pixels, and of the core's system RAM), and per RAM variable a
byte (1 if it decoded) and its value (`i64`, 0 when it didn't). Checks follow every
`checks_every`-th frame and the last frame.

The replayer compares each check when it reaches its frame and stops at the first difference
with `DesyncError` (`gth_record::Error::Desync`) naming the frames since the previous check,
`replay desynced between frames 721 and 780`, since the first frame that differs is one of
them. With `checks_every = 1` that is the frame itself, `replay desynced at frame 777`, at a
cost of 24 bytes plus 9 per RAM variable per frame before compression. A frame's own state
can differ only in what the hashes don't cover (CPU registers, video RAM not yet shown), so the
first difference a check sees can lag its cause. The decoded variables name what differs
first (`RAM variable score is Some(12), recorded Some(13)`), then the RAM hash, then the frame
hash. The values also let tools validate RAM variables against a replay without running it.
Game RAM in a recording is for validation and rewards, never a policy's input.

## Versioning

`format_version` covers the layout and meaning of every file, and `Recording::open` refuses
versions outside `OLDEST_FORMAT_VERSION..=FORMAT_VERSION`. The stored `game.toml` is parsed with today's `gth-config`, which refuses
unknown fields, so a change to the game.toml schema that old files no longer parse under
(a renamed or removed field, a new required one) is a format change too: bump
`FORMAT_VERSION` and read old recordings through a migration. Adding an optional game.toml
field isn't one. `meta.toml`'s `[core.lock]` is stored as a plain table for the same reason.

| Version | Change | Migration |
|---|---|---|
| 1 | The first format. | |
| 2 | game.toml's required `settle_frames` (docs/games.md, "Settle frames"): the recorder and the replayer settle after loading the start state. | A version 1 `game.toml` (its SHA-1 checked as stored) is read with `settle_frames = 0`, which is how it was recorded. |

`[branch]` and `[[rewind]]` didn't bump the version: they are optional, a recording without
them reads exactly as before, and they don't change what any other file holds (a rewound
recording is still one run from its start state, with every check in place). A build from
before them refuses a recording that has them by name (`meta.toml` refuses unknown fields)
rather than misreading it.

## Recording and replaying

- `gth_record::Recorder::create(dir, core, game, content, info)` snapshots the core's state,
  reloads it (a replay starts by loading it, which clears the frame, so the recording does too;
  a core that repeats its frame until the first check would otherwise hash a different picture;
  the reload settles as the replay's does), and writes everything but `meta.toml`; it refuses a
  core loaded with other settle frames than the game's; `run(core, frames, buttons)` runs one
  agent step frame by frame, recording each; `run_sticky(core, frames, buttons, sticky_prob,
  rng)` does so with sticky actions (`Core::run_sticky`), recording each frame's effective
  mask, the one the core ran with, so the recording replays exactly with no RNG; `finish(core)`
  writes the last check and `meta.toml`. Any error in `run` is terminal: the core may have run
  a frame the recording doesn't hold, so later calls, `finish` included, fail with
  `Error::Failed` and the directory stays incomplete.
- `Recorder::rewind(RewindMeta { frame, cause, clip })` truncates the recording to its first
  `frame` frames (the end of a step) and their checks, rewrites `inputs.bin` and
  `checks.bin` from the copy it keeps in memory (3 bytes a frame), and appends the
  `[[rewind]]` entry. The caller has already put the core in the state after those frames
  (gth-play loads a ring-buffer savestate and replays the log forward), so the recording goes
  on as one run. `set_branch(BranchMeta)` labels a side recording; `inputs()` is the log so
  far.
- `RetroEnv(record=Record(...))` records every episode, from the end of its reset's script or
  state load (so the first observation's frames are in it) to the next reset or `close()`, as
  `<directory>/<game_id>-<UTC time>-<pid>-<n>.gthrec` (`n` numbers the process's recordings);
  `env.recordings` lists the finished ones. Starting a recording reloads the state (above), so
  the first frames match the replay rather than an unrecorded run: on a core that repeats its
  frame, the first observation is blank. It runs through `_native.Core.start_recording` and
  `finish_recording`, which record every frame `step` and `run_inputs` run. The no-op start
  (`noop_max`) is a step of its own, and under sticky actions (`sticky_prob`) a step's frames
  hold the masks they ran with, which may be the previous frame's rather than the action's. A
  failed write raises from the `step`, `reset` or `close()` that ran into it and leaves that
  recording incomplete; the next episode records again. Human play (gth-play, P6) will use the
  same `Recorder`.
- `gth_record::Recording::open(dir)` reads and checks a recording; `Replayer::load(recording,
  core_path, content, system_dir)` loads the core with the recording's core options and settle
  frames, checks the core version, the content (above) and that every RAM variable is in the
  core's memory, and loads the start state. `step()` replays one agent step, `run_to_end()` the
  rest. `run_until_terminated(game, cap)` replays frame by frame until a game config's
  `episode.terminate` first holds, `cap` frames are replayed or the recording ends, and
  returns where it stopped with that config's RAM variables there (the config may be today's
  `games/<id>/game.toml` rather than the recording's own, so its variables are checked against
  the core's memory first); `gth reference-scores` scores human credits with it
  (docs/evaluation.md, "Human reference"). `_native.Replayer` (`gtharness.record.Replayer`)
  adds an observation pipeline: its `step()` returns each step's joypad mask (its first
  frame's), frame count and last frame at any size, crop and rotation, and it has
  `run_until_terminated(config, cap)`, which with `cap` at most the frames replayed returns the
  RAM without replaying.

A 60 fps minute of random actions at frameskip 4 is about 5.5 KB on the test core (1.8 KB of
inputs, 2 KB of checks, the rest `meta.toml` and `game.toml`). A real core's start state
comes on top: zstd shrinks most consoles' states to tens of kilobytes.

## Scripted captures

`gth capture DATA_DIR GAME --script attract|random-play --frames N --frameskip K --out DIR
--license L --checks-every C [--seed S]` (`gtharness.capture.capture`) records a game from its
reset with no player: an input script chooses every step's RetroPad mask up front, and
`_native.Core.run_inputs` plays them through the recorder, about a minute of game time per
call, so the frames run in Rust. The recording is an ordinary one (`recorder = "scripted"`),
`N` frames long counting the reset's first observation (`N` is a multiple of `K`, every step
`K` frames), and replays and exports like any other. Its `player` names the script, its
version and its seed (`attract-v1`, `random-play-v1 seed=7`): replay needs only
`inputs.bin`, so they say where the inputs came from: the same arguments, game.toml and
numpy with the same script version (`gtharness.capture.SCRIPT_VERSIONS`, one per script,
bumped when its inputs change) record the same inputs. Options are checked before the core
loads (`ConfigError`, exit 2; a core that fails to load exits 2 too), and
a capture that fails or is interrupted leaves its recording incomplete.

| Script | Inputs |
|---|---|
| `attract` | Nothing held, ever: boot, then the game's attract mode and its demo play. Takes no seed. |
| `random-play` | A credit cycle every 480 frames from the first step (coin, that is RetroPad select, for 8 frames, 16 frames of nothing, start for 8, 16 of nothing), which starts a game from attract mode and continues one at game over; between them, random actions from the game's action space (docs/actions.md), each held 8 to 48 frames: a uniform direction, the shot slot with probability 0.8 and the bomb slot with 0.05 (when the game maps one). Needs `--seed` (numpy's `default_rng`) and a game that maps `shot`. |

Lengths round up to whole steps. Coin and start map to no slot, so the samples they follow
are unlabelled and a shard drops them (16 frames in 480, a thirtieth of `random-play`'s, at
`--frameskip 1`); every frame's sprite list stays. Export attract captures with `--filter all`: `playing`
drops everything before the first input, which is all of an attract capture.

The Psikyo games' captures for the sprite-label experiments (two each of ten minutes, at the
board's 59.3 fps 36,000 frames) are made and exported on the Mac with:

```sh
GTH_ROM_DIR=~/roms uv run gth capture data gunbird --out runs/captures --script attract \
    --frames 36000 --frameskip 1 --license CC-BY-NC-4.0 --checks-every 60
GTH_ROM_DIR=~/roms uv run gth capture data gunbird --out runs/captures --script random-play \
    --seed 0 --frames 36000 --frameskip 1 --license CC-BY-NC-4.0 --checks-every 60
GTH_ROM_DIR=~/roms uv run gth export-shards data runs/captures/*.gthrec --out runs/shards \
    --obs-size 224 320 --crop none --orientation 0 --frameskip 1 --filter all \
    --chunk-samples 256 --sprites yes --maxpool no --grayscale no
```

(and the same for `s1945`). Native size and every frame keep the sprite boxes in observation
pixels and every frame's picture beside its list. Recordings (their start state) and shards
(their frames) are made from the ROM, so they never enter git (`runs/` is git-ignored,
docs/roms.md).

## Compression

Every file but the TOML ones is zstd at level 9 (`zstd` crate, bundled libzstd).

| | zstd | lz4 | none |
|---|---|---|---|
| Size of a minute of random inputs at frameskip 4 (10.8 KB raw) | 2.4 KB (level 9) | 5.9 KB (level 9 or 1) | 10.8 KB; a megabyte state stays a megabyte |
| Speed | Level 9 compresses a megabyte state in tens of milliseconds, once per episode; decompression is far faster than replay | Several times faster | No cost |
| Dependency | `zstd` crate, builds libzstd from C (a C compiler, present on every CI runner) | `lz4_flex`, pure Rust | None |
| Tooling | `zstd -d` reads the files | `lz4` CLI | Any |

zstd: recordings are written once and kept forever, and the start state dominates their size,
so ratio matters more than the speed lz4 offers, which replay (emulation) never needs. Level 9
is a middle ground: higher levels gain little on these files and slow the start state at each
episode start.

## Training shards

Training reads shards rendered from recordings, not recordings. A shard is a sequence of
samples at the model's rate and size; the same format serves video labelled by an inverse
dynamics model (IDM, separate plan), so both sources train one model. `gth_record::export`
(`crates/gth-record/src/shard.rs`) writes one shard per recording by replaying it.

### Container

One shard is one `.npz` file: a zip archive whose members are stored uncompressed, so
`np.load(path)` opens it with nothing but numpy. Its members:

| Member | Holds |
|---|---|
| `frames_00000.zst`, `frames_00001.zst`, ... | The frames, `chunk_samples` samples per member (the last holds the rest): one zstd frame (level 3) of the raw `uint8[n, H, W, C]` pixels (`C` is `channels`: RGB, or one gray byte), row-major. `np.load` returns a member that isn't `.npy` as bytes; `_native.decode_frames` decodes it. |
| `direction.npy`, `buttons.npy`, `ram.npy`, `episode_frame.npy`, `segment.npy`, `confidence.npy` | The per-sample arrays (below). |
| `frame_direction.npy`, `frame_buttons.npy`, `frame_labelled.npy` | The per-frame arrays (below). |
| `sprite_offsets.npy`, `sprites_00000.zst`, ... | Only with sprite lists: every replayed frame's hardware sprite list (below, "Sprite labels"). |
| `meta.json` | The per-shard fields (below). |

Frames are chunked rather than stored one per member, because consecutive frames of a game
are nearly identical and zstd compresses a run of them far better than each alone; the chunk
size is the trade-off between that ratio and how much a reader decodes to reach one sample.
The labels are tiny and stored as plain `.npy`, so tools read them (to count actions, sum
returns, pick episodes by score) without touching a frame.

| | npz (zip of `.npy` and zstd members) | WebDataset (tar, one member per sample and field) |
|---|---|---|
| Sequential read speed | Same: stored members are plain file reads; zstd decoding dominates either way | Same |
| Random access | The zip's central directory locates any member directly: one chunk or the labels alone | A tar has no index: reaching a member scans the headers before it, or needs a separate index |
| Streaming from disk | Needs a seekable file (the directory is at the end); fine on local disks, where shards live (Mac `runs/`, TheaterPC `D:`) | Built for it: reads front to back, also from pipes and object stores |
| Dependencies | numpy and the stdlib (`zipfile`) to read; `rawzip` (already a `gth-config` dependency, no dependencies of its own) to write | stdlib `tarfile` to read, or the `webdataset` package for its pipeline; a tar writer (`tar` crate) to write |
| Windows | Native (`zipfile`, Explorer) | Native (`tarfile`, `tar.exe`) |
| Fit with chunked frames | Any member layout | Its per-sample grouping (`<key>.<field>`) fights chunks spanning many samples |

npz: shards live on local disks next to the trainer, where random access to a chunk or to the
labels matters more than streaming, and it adds no dependency.

### Per sample (one agent step)

A sample is an observation and the action the player chose after seeing it (below, "Labels
and samples").

| Field | Type | Meaning |
|---|---|---|
| `frame` | `uint8[H, W, C]` | The observation at the model size, RGB or gray (`channels`), as `RetroEnv` with the same `maxpool` and `grayscale` observes it (in the frame chunks). |
| `direction` | `uint8` | The action after it: 0 neutral, 1-8 clockwise from up (docs/actions.md). |
| `buttons` | `uint8` | Bit i = slot i held (`shot`, `bomb`, `aux1`, `aux2`). |
| `ram` | `int64[V]` | The game's RAM variables at the observation (`ram_names` names the columns), for rewards, validation and analysis only, never a policy's input; absent for video. |
| `episode_frame` | `uint64` | Frames replayed since the recording's start state at the observation (1-based, like `checks.bin`'s frames; `RetroEnv`'s `info["episode_frame"]` also counts the reset script's frames, which come before a recording starts). The label is frame `episode_frame + 1`'s action. |
| `segment` | `uint32` | Consecutive samples share a segment; it goes up at every gap (a sample dropped as unlabelled or by the filter), so a sequence model never pairs samples across one. Starts at 0. |
| `confidence` | `float32` | The label's confidence: 1 for `replay`. |

### Per frame

Every replayed frame's action, `frames` long (index f - 1 is frame f), for models that
work at the emulator's rate or need the presses between observations; sample i's label is
`frame_*[episode_frame[i]]`.

| Field | Type | Meaning |
|---|---|---|
| `frame_direction`, `frame_buttons` | `uint8` | The frame's action, as `direction` and `buttons`; neutral when unlabelled. |
| `frame_labelled` | `bool` | False when the frame's mask holds a button no slot maps (start, select). |

### Per shard (`meta.json`)

| Field | Meaning |
|---|---|
| `shard_version` | `gth_record::SHARD_VERSION` (3). Version 2 is version 3 without `maxpool`, `grayscale` and `channels` (RGB, not pooled); version 1 is version 2 without the `sprite_*` fields and members. `Shard` reads all three. |
| `source_id` | The recording's directory name, or the video's id. |
| `episode_id` | The episode's index in its source: 0 for a recording, which holds one episode. |
| `game_id`, `fps` | From the recording. |
| `frameskip` | Frames per sample, or `null` when the recorded steps it follows vary in length (`episode_frame` then gives the step ends). |
| `obs_size`, `crop`, `orientation`, `interpolation` | The observation pipeline: `[height, width]`, `[x, y, width, height]` or `null`, degrees clockwise, `area`, `bilinear` or `bicubic`. |
| `maxpool` | Whether each observation is the per-channel maximum of its step's last two frames (`RetroEnv`'s `maxpool`; at a step of one frame, the other is the previous step's last). A savestate holds no frame, so at a step of one frame the first sample pools with black where the env pooled with the reset's last frame; every later sample matches. |
| `grayscale`, `channels` | Whether observations are BT.601 gray (`RetroEnv`'s `grayscale`), and the bytes per pixel: 1 when gray, else 3. Readers size frames by `channels`; `batches` and `Windows` refuse shards of different `(height, width, channels)`. |
| `ram_names` | The names of `ram`'s columns (the recording's `game.toml` variables, in name order). |
| `label_source`, `idm_model` | `replay` or `idm`, and for `idm` the model's id (`null` otherwise). |
| `filter` | The sample filter (below). |
| `samples`, `frames` | Sample count, and frames replayed (the per-frame arrays' length). |
| `chunk_samples`, `frame_chunks` | Samples per frame chunk, and chunk count. |
| `terminated` | Whether `episode.terminate` holds at the last sample's observation: the shard ends at game over. |
| `sprite_hw` | The board whose sprite lists the shard holds (the recording's `game.toml` `sprite_hw`), or `null` without sprite lists; then the other `sprite_*` fields are `null` too. |
| `sprite_screen` | `[width, height]` of the native screen the sprite coordinates are in (the board's screen, which binding checks is the core's `base_width` and `base_height`, so a rotated core output is refused): before `crop`, `orientation` and the resize. |
| `sprite_chunk_frames`, `sprite_chunks` | Frames per sprite chunk (`gth_record::SPRITE_CHUNK_FRAMES`, 4096; the last holds the rest), and chunk count. |
| `recording` | The recording's `meta.toml` as JSON (license, player, core, content, the recording harness build). |
| `harness` | The exporting harness build (`_native.build_info()`). |

A per-slot confidence (direction and each button) may replace `confidence` if the IDM can
give one; that is a new `shard_version`.

### Sprite labels

Shards can hold each replayed frame's hardware sprite table: what the board's sprite chip
drew, one record per sprite, as a training target (for an auxiliary head that locates the
player, enemies and bullets). Like RAM, it is never a policy's input: `batches` leaves it out.

The lists come from the driver's own buffers, not RAM: a game's `sprite_hw` (docs/games.md)
names the board, and `gth-sprites` decodes its buffers through the core's named state areas
(docs/libretro.md, "Named areas"). Boards so far:

| `sprite_hw` | Games | Core |
|---|---|---|
| `psikyo` | Psikyo's 68EC020 boards: `gunbird`, `s1945` (and the other `psikyo.cpp` sets) | our FBNeo fork (docs/cores.md, "Forks") |
| `testcore` | the test core, with `gth_testcore_sprites = "on"` (docs/libretro.md, "Test core") | `gth_testcore` |

Boards double-buffer the list: Psikyo's driver copies sprite RAM into one buffer at each
vblank and draws the other. The decoder reads the buffer the next frame draws, so the list
read just before frame f runs is exactly what frame f shows. It reads before every frame, so
the lists don't depend on `frameskip` or the filter: `sprite_offsets` lines up with the
per-frame arrays, and a later per-frame (or per-sample) field, such as audio, sits beside it
without touching it.

Members:

| Member | Holds |
|---|---|
| `sprite_offsets.npy` | `uint64[frames + 1]`: frame index i's records (frame i + 1, as the per-frame arrays) are records `offsets[i]` to `offsets[i + 1]` of the whole list, so frame index i's list is empty when they're equal. |
| `sprites_00000.zst`, ... | The records of `sprite_chunk_frames` frames each (chunk c: frame indices `c * sprite_chunk_frames` onwards, records from `offsets[c * sprite_chunk_frames]`), one zstd frame (level 3) each; `_native.decode_sprites` decodes one. |

A record is 18 bytes, little-endian and packed (`gtharness.shards.SPRITE_DTYPE`,
`gth_sprites::Sprite::encode`), in the board's list order:

| Field | Type | Meaning |
|---|---|---|
| `x`, `y` | `int16` | The box's top-left corner in native screen pixels (`sprite_screen`); negative or past the screen when it hangs off an edge. |
| `width`, `height` | `uint16` | The box as drawn, after the board's zoom. |
| `code` | `uint32` | The board's tile code of the first tile (Psikyo: the tile LUT index). |
| `palette` | `uint16` | The board's palette number (Psikyo: 0-31). |
| `cols`, `rows` | `uint8` | Tiles across and down (Psikyo: 1-8). |
| `priority` | `uint8` | The board's priority level (Psikyo: 0-3, FBNeo's layer order). |
| `flags` | `uint8` | Bit 0 flip x, bit 1 flip y, bit 2 visible (the box overlaps the screen; boards keep off-screen sprites in their lists), bit 3 zoomed. Visible says where the box is, not that anything shows: a box of transparent tiles, or one drawn under a higher tilemap, is still visible. |

Coordinates stay native: mapping them through the shard's crop, orientation and resize to the
observation is the consumer's (training, P10), so one shard serves any target layout.
For Psikyo, a sprite of `cols` x `rows` 16-pixel tiles is `16 * cols` wide, or with zoom
`z = xzoom` it starts `(cols * z + 3) / 4` pixels right of `x` and is `cols * (32 - z) / 2`
wide (likewise down), as FBNeo's `PsikyoSpriteRender` draws it.

`--sprites yes` refuses (`ConfigError`, before anything runs) a recording whose `game.toml`
has no `sprite_hw`; a core that can't give the list (a stock core without the named-area
interface, or a game whose board areas it lacks) fails that recording's export with the
reason, leaving no shard.

### Labels and samples

Observations are taken at the recording's own step ends (`Steps::Recorded`) or every k
frames from the start state (`Steps::Every(k)`, the model's rate whatever the recording's;
`frameskip` in `meta.json`). Each observation is the frame the core showed at that point,
and its sample's label is the action held during the next frame: the first thing the player
did after seeing it. That's the same at every rate, so a label never comes from frames
after the decision: the policy learns `frame -> action` from each sample alone, with no
shifting. The last observation has no next frame and isn't a sample.

A frame's action comes from its mask with the recording's `game.toml`
(`ActionSpace::action_of`, docs/actions.md): the d-pad bits give the direction (opposites
cancel), and a slot is held when any RetroPad button mapped to it is. When `RetroEnv` records
without sticky actions, a step holds one mask for all its frames, so at the recorded rate the
label is the next step's action; with them, the label is what the core ran on the next step's
first frame, which may still be the previous action. At a coarser rate, presses that start and
end between two observations appear only in the per-frame arrays.

A sample whose next frame's mask holds a button that maps to no slot (start, select) has no
label and is dropped (`unlabelled`). A sample filter (`gth_record::SampleFilter`) then
chooses which labelled samples a shard keeps (`filtered` counts the rest):

- `all` keeps every one.
- `playing` keeps from the observation before the first non-neutral action (the one the
  first input answers) through game over, and drops the attract mode or idle start before
  and anything after. Game over is the first observation where `episode.terminate` holds
  after it hasn't held at least once from the start (the observation before the first input
  included), so a title screen whose RAM looks like game over (`lives == 0` until the game
  sets them) doesn't end the episode at once; the game-over observation is kept.

A filter sees each sample's action (`None` when unlabelled), whether it terminates, and its
`episode_frame`, so new ones are a small trait implementation. Every gap a drop leaves bumps
`segment`. Dropped observations are rendered but never written.

Episode returns aren't stored: the reward is a choice made at training time (docs/
architecture.md, "Observations and RAM"), so the trainer computes it from `ram`.

An export writes to `<shard>.partial` and renames it when complete, so a shard that exists
is whole; a desync or any error removes the partial file.

### Exporting and reading

`gth export-shards DATA_DIR RECORDING... --out DIR` (or `gtharness.shards.export_shards`)
writes `DIR/<recording>.npz` for each recording. Every choice that shapes the samples is a
required option: `--obs-size H W`, `--crop game|none|X Y W H`, `--orientation game|0|90|270`
(`game` takes each recording's `game.toml`), `--frameskip N|recorded`, `--filter
all|playing`, `--chunk-samples N`, `--sprites yes|no` (above, "Sprite labels"),
`--maxpool yes|no` and `--grayscale yes|no` (as `RetroEnv`'s); only `--interpolation`
defaults (`area`, as `RetroEnv`). Content, BIOS files and the core are found as `RetroEnv.from_game` finds them.
A shard doesn't carry `[branch]`: to keep rewind clips (deaths) out of a training set, or
to label them, pick the recordings by their `meta.toml` before exporting.
Each recording exports in its own spawned process (one core per process, one process per
recording), `--jobs` (default: the CPU count) at a time. A recording that fails, or whose
process dies, leaves no shard (the parent removes a partial one) and doesn't stop the
others; existing shards are never replaced. Options are checked before anything runs
(`ConfigError`; the command exits 2). Scripts that call
`export_shards` need the usual `if __name__ == "__main__":` guard of spawned processes.

Shards go to `D:\research\gth\shards` on TheaterPC and under the git-ignored `runs/` on
the Mac.

`gtharness.shards.Shard(path)` reads one shard: `meta`, the per-sample arrays (`direction`,
`buttons`, `ram`, `episode_frame`, `segment`, `confidence`), the per-frame arrays
(`frame_direction`, `frame_buttons`, `frame_labelled`) and `frames(chunk)`; with sprite
lists, `sprite_hw`, `sprite_screen`, `sprite_offsets`, `sprite_chunk(chunk)` (a chunk's
records) and `sprites(frame_index)` (one frame's). `batches(paths,
batch_size, rng=..., buffer_chunks=...)` yields every sample once as dicts of numpy arrays:
with a `numpy.random.Generator` it visits frame chunks in random order and shuffles within
`buffer_chunks` decoded chunks; with `rng=None` it keeps the order. Batches hold `frame`,
`direction`, `buttons`, `episode_frame`, `segment`, `confidence` and the `shard` and `sample`
indices, but not `ram`: a reward reads `Shard.ram` at those indices, and the policy never
sees RAM.

### Export throughput

Measured on the Mac (Apple M4, release build, one process) with the test core: one
18,000-step episode at frameskip 4 (72,004 frames), crop and orientation from the game,
`area`, 256 samples per chunk, timed around `export_shards` (process start included):

| Observation | Samples/s | Frames replayed/s | Bytes/sample | Decoding (`Shard.frames`) |
|---|---|---|---|---|
| 128 x 128 | 6,150 | 24,600 | 162 | 337,000 samples/s |
| 84 x 84 | 8,250 | 33,000 | 184 | 332,000 samples/s |

The test core replays at 106,000 frames/s alone, so rendering and compressing cost about
125 µs per 128 x 128 sample. A real core emulates at a few thousand frames/s or less, so an
export runs at about the core's replay speed and scales with `--jobs`. The test core's flat
frames compress far better than a game's: expect real shards to be much larger per sample.
