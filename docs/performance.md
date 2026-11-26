# Performance

How fast the harness runs real cores, how big and fast their savestates are, and whether
their recordings replay: `gth bench` (`python/gtharness/bench.py`) measures all of it, and
this page keeps each machine's results and the decisions taken from them.

## Running the benchmark

Build the extension in release first (`maturin develop` builds debug, and the benchmark
warns about a debug build), fetch the pinned cores, and keep other heavy work (builds, other
benchmarks) off the machine while it runs:

```sh
uv run maturin develop --uv --release
uv run gth cores fetch data
GTH_ROM_DIR=~/roms uv run gth bench data --game gunhed --game zanac --game lightening-force \
    --content fbneo=$HOME/roms/bgaregga.zip --out runs/bench --json runs/bench.json
```

`--game ID` takes a game in `games/` (its content is found by SHA-1 in the ROM directory,
docs/roms.md); `--content CORE=PATH` takes content that has no game config yet, such as an
arcade set. It prints the tables below in Markdown, progress on stderr, and with `--json` the
raw results. The exit status is 1 when a target desyncs or a reloaded savestate diverges,
and 2 on a usage or config error. Recordings go under `--out`: they are made from the user's
ROMs, so keep them out of git (`runs/` is ignored).

On TheaterPC (Windows, PowerShell), the same command from a checkout, with its ROM
directory:

```powershell
uv run maturin develop --uv --release
uv run gth cores fetch data
$env:GTH_ROM_DIR = "D:\research\roms"
uv run gth bench data --game gunhed --game zanac --game lightening-force `
    --content fbneo=D:\research\roms\bgaregga.zip --out runs\bench --json runs\bench.json
```

Then add its tables to "Results" below, under the machine.

### What it measures

The benchmark runs each target with its own game config (`bench.target_config`): the game's
core, content, core options, turn and crop, no RAM variables, the four action slots on B, A,
Y and X, and a reset script that coins and presses start a few times (about 13 seconds), so
play leaves the title screen in most games. It measures the same thing before and after a
game's own `game.toml` is written. Actions are uniformly random flat actions
(docs/actions.md). Every measurement runs in spawned processes, one core per process.

- **Single process**: one `RetroEnv` with 256x256 observations, at frameskip 1, 2 and 4, in
  five timed batches of 1000 steps (medians). Per step: the whole `env.step`; the native step
  (`_native.Core.step` alone, which emulates `frameskip` frames and renders the observation);
  of that, emulating (`Core.run_inputs`, no render) and rendering (the difference); and the
  Python overhead. Every batch of every frameskip starts from the state right after the
  reset, and the three loops run the same frames from it, so neither the rows nor the loops
  differ by game scene. The Python overhead is timed inside
  `env.step`: a wrapper around the core times its `step` call, and the rest of `env.step`
  (action checks and lookup, the `read_ram` call, the reward, the info dict) is the overhead.
  The wrapper's own cost counts as overhead, so the figure errs high. A native call's
  boundary (a `Core.frames_run` read) costs about 0.02 µs, so timing native calls from Python
  is accurate to well under a microsecond.
- **Processes at once**: N environments, each in its own process, stepping for 5 seconds
  after all are ready (a barrier); total frames/s and per process. N is 1, 2, 4 and the CPU
  count minus one, from `os.cpu_count()`.
- **Savestates**: size, and the median time of 100 saves (`Core.serialize`) and loads
  (`Core.unserialize`). "Reload matches": 600 steps after loading the state give the same
  observations as the run that played on from it before any load, so a savestate that
  misses some of the core's state shows up here.
- **Determinism**: 36000 frames (10 minutes) of random actions at frameskip 4, recorded
  with a desync check on every frame (`checks_every = 1`, docs/recording-format.md), then
  replayed in a fresh process, as shard export replays (so state a core keeps between loads
  in one process can't hide a difference). The recorded frames include the reset's first
  observation (4 frames). The action space has no start or coin, so after a game over the
  rest of the run may be continue or attract screens that ignore the input: the replay
  still checks the emulation frame by frame, but less of the input path than ten minutes of
  play would.

## Results

### Apple M4 Mac, 2026-11-26

Machine: Apple M4, 10 CPUs (4 performance, 6 efficiency), macOS 26.6.2; harness 023bbf4 (release).

| Target | Core | Version | Pinned build |
|---|---|---|---|
| gunhed | `mednafen_pce_fast` | `v1.31.0.0 2de1ce1` | yes |
| zanac | `fceumm` | `(SVN) e0f3086` | yes |
| lightening-force | `genesis_plus_gx` | `v1.7.4 b9c2316` | yes |
| bgaregga | `fbneo` | `v1.0.0.03 261110 GITa836e52` | yes |

Single process, 256x256 observations: frames/s and the time of one step in µs.

| Target | Frameskip | Frames/s | Env step | Native step | Emulate | Render | Python | Python share |
|---|---|---|---|---|---|---|---|---|
| gunhed | 1 | 3817 | 262 | 262 | 157 | 105 | 1.0 | 0.4% |
| gunhed | 2 | 4649 | 430 | 428 | 327 | 102 | 1.2 | 0.3% |
| gunhed | 4 | 5364 | 746 | 746 | 644 | 102 | 1.3 | 0.2% |
| zanac | 1 | 5833 | 171 | 169 | 129 | 41 | 0.9 | 0.5% |
| zanac | 2 | 6752 | 296 | 297 | 257 | 41 | 0.9 | 0.3% |
| zanac | 4 | 7283 | 549 | 553 | 507 | 45 | 0.9 | 0.2% |
| lightening-force | 1 | 1522 | 657 | 654 | 242 | 412 | 1.4 | 0.2% |
| lightening-force | 2 | 2247 | 890 | 888 | 475 | 413 | 1.5 | 0.2% |
| lightening-force | 4 | 2880 | 1389 | 1385 | 973 | 412 | 1.5 | 0.1% |
| bgaregga | 1 | 1724 | 580 | 580 | 247 | 333 | 1.0 | 0.2% |
| bgaregga | 2 | 2425 | 825 | 828 | 481 | 348 | 1.0 | 0.1% |
| bgaregga | 4 | 2795 | 1431 | 1423 | 1100 | 323 | 1.1 | 0.1% |

Processes at once, 256x256 observations: total frames/s (and per process).

| Target | Frameskip | N = 1 | N = 2 | N = 4 | N = 9 |
|---|---|---|---|---|---|
| gunhed | 1 | 3629 (3629) | 7601 (3801) | 14262 (3565) | 22059 (2451) |
| gunhed | 2 | 5223 (5223) | 10107 (5053) | 18990 (4747) | 28949 (3217) |
| gunhed | 4 | 6368 (6368) | 12417 (6208) | 22700 (5675) | 34107 (3790) |
| zanac | 1 | 5866 (5866) | 11588 (5794) | 20855 (5214) | 31217 (3469) |
| zanac | 2 | 6719 (6719) | 13175 (6588) | 23896 (5974) | 36148 (4016) |
| zanac | 4 | 7185 (7185) | 14291 (7146) | 26034 (6508) | 39184 (4354) |
| lightening-force | 1 | 1612 (1612) | 3184 (1592) | 5837 (1459) | 9742 (1082) |
| lightening-force | 2 | 2308 (2308) | 4584 (2292) | 8722 (2181) | 13934 (1548) |
| lightening-force | 4 | 2920 (2920) | 5789 (2895) | 11225 (2806) | 17888 (1988) |
| bgaregga | 1 | 1570 (1570) | 3101 (1550) | 5483 (1371) | 8940 (993) |
| bgaregga | 2 | 2122 (2122) | 4192 (2096) | 7481 (1870) | 12274 (1364) |
| bgaregga | 4 | 2523 (2523) | 5003 (2502) | 9228 (2307) | 14863 (1651) |

| Target | Savestate bytes | Save µs | Load µs | Reload matches |
|---|---|---|---|---|
| gunhed | 80586 | 10 | 73 | yes, 600 frames |
| zanac | 21976 | 2 | 9 | yes, 600 frames |
| lightening-force | 1036288 | 35 | 15 | yes, 600 frames |
| bgaregga | 126044 | 14 | 6 | yes, 600 frames |

| Target | Frames recorded | Replayed | Desync | Record s | Replay s |
|---|---|---|---|---|---|
| gunhed | 36004 | 36004 | none | 7.6 | 6.5 |
| zanac | 36004 | 36004 | none | 8.1 | 7.6 |
| lightening-force | 36004 | 36004 | none | 12.8 | 11.6 |
| bgaregga | 36004 | 1 | frame 1 | 17.2 | 0.0 |

The numbers are from one run with no builds running (`pgrep -f '[c]argo|[r]ustc'` logged
every second). Earlier runs on the same machine agreed within about 1.5% for frames/s; save
and load times vary by a few µs between runs. What they show:

- Rendering the 256x256 observation costs a fixed amount per step (40-410 µs, depending on
  the core's frame size and pixel format), while emulating scales with the frames run, so
  frameskip raises frames/s most where rendering is a large share of the step: from
  frameskip 1 to 4, Lightening Force gains 1.9x, Battle Garegga 1.6x, Gunhed 1.4x and Zanac
  1.25x.
- Processes scale nearly linearly up to the four performance cores (each process keeps
  87-98% of its single-process rate at N = 4). From 4 to 9 processes the total rises by
  50-67%: each of the five extra processes, on efficiency cores, adds 40-54% of a
  performance core's rate. The parallel runs play on from the reset without restoring a
  state, so their scenes differ from the single-process table's (Gunhed at frameskip 4 runs
  6368 frames/s there against 5364 here).
- Savestates are small (22 KB to 1 MB) and take 2-73 µs to save or load, so per-step
  checkpoints for resets or branching are cheap.

### TheaterPC

Pending: TheaterPC wasn't reachable when this was written. Run the benchmark there as
described above and add its section.


## Decisions

### P5: batching stays in Python

The trigger: if the Python overhead per step is more than 10% of the step's time at
frameskip 1, P5 moves batching into Rust (a shared-memory ring); otherwise P5 stays a
multiprocessing vector env with shared-memory observations.

**Outcome: below the trigger, so P5 stays a multiprocessing vector env with
shared-memory observations.** On the Mac the overhead is 0.9-1.5 µs per step, 0.2-0.5% of the
step at frameskip 1 (and less at higher frameskip); even Zanac, the cheapest step at 171 µs,
would need 17 µs of overhead to reach 10%. Recheck when TheaterPC's numbers are in, or when
`env.step` gains per-step Python work (RAM variables and a reward, which the benchmark config
leaves out, add `read_ram` work proportional to the variables read).

## Findings

### FBNeo recordings desync at frame 1 on replay

Battle Garegga (`fbneo`, `bgaregga.zip`) records fine, but its replay in a fresh process
stops at frame 1: the frame hash differs while RAM matches. A diagnostic run showed that
only the first frame after loading a savestate differs, and only when the core in that
process has run fewer than about 60 frames before the load; after that, every frame matches,
and reloading in the same process matches the live run for 600 frames (the savestate table).
The other three cores replay all 36004 frames. The likely cause is FBNeo drawing the first
frame after a load from video state that isn't in the savestate and that a fresh core hasn't
initialised yet. Until it's fixed, FBNeo recordings don't replay, and a fresh process's
first observation after restoring a state can differ from the recorded one. Follow-up: find
the state (or run a few frames before loading in the replayer and env reset for cores that
need it).