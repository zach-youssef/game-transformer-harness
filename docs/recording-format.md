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
| `format_version` | `gth_record::FORMAT_VERSION` (1). Readers refuse any other version, checked before the other fields. |
| `game_id` | The game's directory under `games/`. |
| `game_config_sha1` | SHA-1 of `game.toml`. |
| `recorder` | `human` (gth-play), `policy`, `scripted` (tests, random or scripted rollouts) or `converted` (from a MAME `.inp`). |
| `player` | A person's handle, or the policy and checkpoint that played. |
| `license` | The license the recording is shared under. The user's own play is shareable non-commercially (D8), e.g. `CC-BY-NC-4.0`. |
| `created_unix` | Seconds since the Unix epoch when recording started. |
| `fps` | The core's frame rate (`retro_get_system_av_info`). |
| `frames`, `steps` | Emulated frames and agent steps in `inputs.bin`. |
| `checks_every` | Frames between two checks. |
| `[core]` | `name` (game.toml's `core`), `library_name` and `library_version` (`retro_get_system_info`; a replay refuses a core that reports others), and `lock`, the core's `cores.lock` entry, when the recorder has one (`RetroEnv`'s recordings don't until P1 wires up `cores.lock`; kept as a plain table that replays don't read, so it survives changes to the lock's schema). |
| `[content]` | `file` and `sha1` of what identifies the content the core loaded: the content file, or for a clone `in_parent` (docs/games.md), whose path doesn't exist, the parent archive holding it. A replay refuses content with another SHA-1, no content when the recording has some, and content when it has none. Absent when the core loaded none. Not checked: an `in_parent` clone's own files (the archive's SHA-1 covers them, with every other set in it), the other `parents`, and BIOS files, whose hashes are in the stored `game.toml` for the ROM audit (P2). |
| `[start]` | `sha1` and `size` of the uncompressed start state. |
| `[harness]` | The harness build, `_native.build_info()`: version, git commit and dirty flag, target, profile, rustc. |

The recorder, player, license and check interval are required wherever a recording is made
(`RecordingInfo`, `gtharness.record.Record`), so none of them is ever a silent default.

### inputs.bin

The magic `GTHINPUT`, then 3 bytes per frame: the port 0 RetroPad mask as a `u16` (bit n =
`RETRO_DEVICE_ID_JOYPAD_*` n, the input the core saw, d-pad and reset-script buttons
included), and a flags byte whose bit 0 marks the last frame of an agent step (the frame its
observation shows); the other bits must be 0. The last frame always ends a step.

The mask, not the action, is the canonical input: it is what the core consumed, so replay is
exact whatever action space produced it. The flags keep the agent's steps: frameskip is the
distance between step ends, so frame-rate and model-rate data both come from one recording.
Unified-slot labels for training are derived from the mask with the recording's own
`game.toml` (below).

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
any other version. The stored `game.toml` is parsed with today's `gth-config`, which refuses
unknown fields, so a change to the game.toml schema that old files no longer parse under
(a renamed or removed field, a new required one) is a format change too: bump
`FORMAT_VERSION` and read old recordings through a migration. Adding an optional game.toml
field isn't one. `meta.toml`'s `[core.lock]` is stored as a plain table for the same reason.

## Recording and replaying

- `gth_record::Recorder::create(dir, core, game, content, info)` snapshots the core's state,
  reloads it (a replay starts by loading it, which clears the frame, so the recording does
  too; a core that repeats its frame until the first check would otherwise hash a different
  picture), and writes everything but `meta.toml`; `run(core, frames, buttons)` runs one
  agent step frame by frame, recording each; `finish(core)` writes the last check and
  `meta.toml`. Any error in `run` is terminal: the core may have run a frame the recording
  doesn't hold, so later calls, `finish` included, fail with `Error::Failed` and the directory
  stays incomplete.
- `RetroEnv(record=Record(...))` records every episode, from the end of its reset's script or
  state load (so the first observation's frames are in it) to the next reset or `close()`, as
  `<directory>/<game_id>-<UTC time>-<pid>-<n>.gthrec` (`n` numbers the process's
  recordings); `env.recordings` lists the finished ones. Starting a recording reloads the
  state (above), so the first frames match the replay rather than an unrecorded run: on a core
  that repeats its frame, the first observation is blank. It runs through
  `_native.Core.start_recording` and `finish_recording`, which record every frame `step` and
  `run_inputs` run. A failed write raises from the `step`, `reset` or `close()` that ran
  into it and leaves that recording incomplete; the next episode records again. Human play
  (gth-play, P6) will use the same `Recorder`.
- `gth_record::Recording::open(dir)` reads and checks a recording;
  `Replayer::load(recording, core_path, content, system_dir)` loads the core with the
  recording's core options, checks the core version, the content (above) and that every RAM
  variable is in the core's memory, and loads the start state. `step()` replays one agent
  step, `run_to_end()` the rest. `_native.Replayer` (`gtharness.record.Replayer`) adds an
  observation pipeline: its `step()` returns each step's joypad mask, frame count and last
  frame at any size, crop and rotation.

A 60 fps minute of random actions at frameskip 4 is about 5.5 KB on the test core (1.8 KB of
inputs, 2 KB of checks, the rest `meta.toml` and `game.toml`). A real core's start state
comes on top: zstd shrinks most consoles' states to tens of kilobytes.

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
| `frames_00000.zst`, `frames_00001.zst`, ... | The frames, `chunk_samples` samples per member (the last holds the rest): one zstd frame (level 3) of the raw `uint8[n, H, W, 3]` pixels, RGB, row-major. `np.load` returns a member that isn't `.npy` as bytes; `_native.decode_frames` decodes it. |
| `direction.npy`, `buttons.npy`, `ram.npy`, `episode_frame.npy`, `segment.npy`, `confidence.npy` | The per-sample arrays (below). |
| `frame_direction.npy`, `frame_buttons.npy`, `frame_labelled.npy` | The per-frame arrays (below). |
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
| `frame` | `uint8[H, W, 3]` | The observation at the model size, RGB (in the frame chunks). |
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
| `shard_version` | `gth_record::SHARD_VERSION` (1). |
| `source_id` | The recording's directory name, or the video's id. |
| `episode_id` | The episode's index in its source: 0 for a recording, which holds one episode. |
| `game_id`, `fps` | From the recording. |
| `frameskip` | Frames per sample, or `null` when the recorded steps it follows vary in length (`episode_frame` then gives the step ends). |
| `obs_size`, `crop`, `orientation`, `interpolation` | The observation pipeline: `[height, width]`, `[x, y, width, height]` or `null`, degrees clockwise, `area` or `bilinear`. |
| `ram_names` | The names of `ram`'s columns (the recording's `game.toml` variables, in name order). |
| `label_source`, `idm_model` | `replay` or `idm`, and for `idm` the model's id (`null` otherwise). |
| `filter` | The sample filter (below). |
| `samples`, `frames` | Sample count, and frames replayed (the per-frame arrays' length). |
| `chunk_samples`, `frame_chunks` | Samples per frame chunk, and chunk count. |
| `terminated` | Whether `episode.terminate` holds at the last sample's observation: the shard ends at game over. |
| `recording` | The recording's `meta.toml` as JSON (license, player, core, content, the recording harness build). |
| `harness` | The exporting harness build (`_native.build_info()`). |

A per-slot confidence (direction and each button) may replace `confidence` if the IDM can
give one; that is a new `shard_version`.

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
cancel), and a slot is held when any RetroPad button mapped to it is. When `RetroEnv`
records, a step holds one mask for all its frames, so at the recorded rate the label is the
next step's action. At a coarser rate, presses that start and end between two observations
appear only in the per-frame arrays.

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
