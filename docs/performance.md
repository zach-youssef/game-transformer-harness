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
    --game bgaregga --out runs/bench --json runs/bench.json
```

`--game ID` takes a game in `games/` (its content is found by SHA-1 in the ROM directory,
docs/roms.md); `--content CORE=PATH` takes content that has no game config yet, such as an
arcade set, with the core's default options and no settle frames (docs/games.md), so a core
that needs them (FBNeo) desyncs at frame 1 that way. It prints the tables below in
Markdown, progress on stderr, and with `--json` the raw results. The exit status is 1 when a
target desyncs or a reloaded savestate diverges, and 2 on a usage or config error.
Recordings go under `--out`: they are made from the user's ROMs, so keep them out of git
(`runs/` is ignored).

On TheaterPC (Windows, PowerShell), the same command from a checkout, with its ROM
directory:

```powershell
uv run maturin develop --uv --release
uv run gth cores fetch data
$env:GTH_ROM_DIR = "D:\research\roms"
uv run gth bench data --game gunhed --game zanac --game lightening-force `
    --game bgaregga --out runs\bench --json runs\bench.json
```

Then add its tables to "Results" below, under the machine.

### What it measures

The benchmark runs each target with its own game config (`bench.target_config`): the game's
core, content, core options, settle frames, turn and crop, no RAM variables, the four action slots on B, A,
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
  (`Core.unserialize`, which includes the game's settle frames). "Reload matches": 600 steps after loading the state give the same
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

Machine: Apple M4, 10 CPUs (4 performance, 6 efficiency), macOS 26.6.2; harness 1180c3b
(release; the run was on an earlier build of the same branch, with the same bench code).
FBNeo was a local build of its pinned commit and patches (`scripts/build-core.sh`, the same
library_version the release reports), measured before the cores workflow released it: its
hash was put in `cores.lock` for the run only, so `gth cores fetch` would take it. Its row
says so until the release's build is measured.

| Target | Core | Version | Pinned build |
|---|---|---|---|
| gunhed | `mednafen_pce_fast` | `v1.31.0.0 2de1ce1` | yes |
| zanac | `fceumm` | `(SVN) e0f3086` | yes |
| lightening-force | `genesis_plus_gx` | `v1.7.4 b9c2316` | yes |
| bgaregga | `fbneo` | `v1.0.0.03 261110 GIT0630faa` | no: local build, pending the release |

Single process, 256x256 observations: frames/s and the time of one step in µs.

| Target | Frameskip | Frames/s | Env step | Native step | Emulate | Render | Python | Python share |
|---|---|---|---|---|---|---|---|---|
| gunhed | 1 | 3822 | 262 | 260 | 155 | 105 | 1.0 | 0.4% |
| gunhed | 2 | 4659 | 429 | 429 | 322 | 107 | 1.1 | 0.3% |
| gunhed | 4 | 5390 | 742 | 743 | 635 | 108 | 1.2 | 0.2% |
| zanac | 1 | 5892 | 170 | 170 | 128 | 42 | 0.9 | 0.5% |
| zanac | 2 | 6733 | 297 | 296 | 254 | 42 | 0.9 | 0.3% |
| zanac | 4 | 7241 | 552 | 556 | 511 | 44 | 0.9 | 0.2% |
| lightening-force | 1 | 1529 | 654 | 653 | 242 | 410 | 1.3 | 0.2% |
| lightening-force | 2 | 2260 | 885 | 884 | 475 | 408 | 1.4 | 0.2% |
| lightening-force | 4 | 2889 | 1385 | 1382 | 973 | 409 | 1.5 | 0.1% |
| bgaregga | 1 | 1721 | 581 | 579 | 247 | 332 | 0.9 | 0.2% |
| bgaregga | 2 | 2427 | 824 | 826 | 483 | 343 | 1.0 | 0.1% |
| bgaregga | 4 | 2800 | 1429 | 1432 | 1106 | 326 | 1.0 | 0.1% |

Processes at once, 256x256 observations: total frames/s (and per process).

| Target | Frameskip | N = 1 | N = 2 | N = 4 | N = 9 |
|---|---|---|---|---|---|
| gunhed | 1 | 3619 (3619) | 7623 (3811) | 14363 (3591) | 21795 (2422) |
| gunhed | 2 | 5227 (5227) | 10075 (5038) | 19181 (4795) | 28216 (3135) |
| gunhed | 4 | 6347 (6347) | 12508 (6254) | 22804 (5701) | 33601 (3733) |
| zanac | 1 | 5892 (5892) | 11574 (5787) | 20947 (5237) | 30604 (3400) |
| zanac | 2 | 6671 (6671) | 13253 (6626) | 24209 (6052) | 35432 (3937) |
| zanac | 4 | 7206 (7206) | 14250 (7125) | 26030 (6507) | 38464 (4274) |
| lightening-force | 1 | 1626 (1626) | 3219 (1609) | 5897 (1474) | 9563 (1063) |
| lightening-force | 2 | 2332 (2332) | 4600 (2300) | 8772 (2193) | 13639 (1515) |
| lightening-force | 4 | 2933 (2933) | 5808 (2904) | 11317 (2829) | 17510 (1946) |
| bgaregga | 1 | 1555 (1555) | 3107 (1553) | 5490 (1373) | 8718 (969) |
| bgaregga | 2 | 2125 (2125) | 4197 (2099) | 7475 (1869) | 11862 (1318) |
| bgaregga | 4 | 2530 (2530) | 4963 (2482) | 9240 (2310) | 14371 (1597) |

| Target | Savestate bytes | Save µs | Load µs | Reload matches |
|---|---|---|---|---|
| gunhed | 80586 | 10 | 74 | yes, 600 frames |
| zanac | 21976 | 2 | 9 | yes, 600 frames |
| lightening-force | 1036288 | 36 | 15 | yes, 600 frames |
| bgaregga | 158836 | 16 | 245 | yes, 600 frames |

| Target | Frames recorded | Replayed | Desync | Record s | Replay s |
|---|---|---|---|---|---|
| gunhed | 36004 | 36004 | none | 7.6 | 6.5 |
| zanac | 36004 | 36004 | none | 8.1 | 7.5 |
| lightening-force | 36004 | 36004 | none | 12.8 | 11.6 |
| bgaregga | 36004 | 36004 | none | 17.1 | 16.6 |

The numbers are from one run with no builds running (`pgrep -f '[c]argo|[r]ustc'` logged
every second). Earlier runs on the same machine agreed within about 1.5% for frames/s; save
and load times vary by a few µs between runs. What they show:

- Rendering the 256x256 observation costs a fixed amount per step (40-410 µs, depending on
  the core's frame size and pixel format), while emulating scales with the frames run, so
  frameskip raises frames/s most where rendering is a large share of the step: from
  frameskip 1 to 4, Lightening Force gains 1.9x, Battle Garegga 1.6x, Gunhed 1.4x and Zanac
  1.2x.
- Processes scale nearly linearly up to the four performance cores (each process keeps
  88-99% of its single-process rate at N = 4). From 4 to 9 processes the total rises by
  46-62%: each of the five extra processes, on efficiency cores, adds 37-50% of a
  performance core's rate. The parallel runs play on from the reset without restoring a
  state, so their scenes differ from the single-process table's (Gunhed at frameskip 4 runs
  6347 frames/s there against 5390 here).
- Savestates are small (22 KB to 1 MB) and take 2-74 µs to save or load, so per-step
  checkpoints for resets or branching are cheap. Battle Garegga's load takes 245 µs because
  it includes its settle frame, one emulated frame (247 µs at frameskip 1 above); see
  "FBNeo and savestates" below.

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
step at frameskip 1 (and less at higher frameskip); even Zanac, the cheapest step at 170 µs,
would need 17 µs of overhead to reach 10%. Recheck when TheaterPC's numbers are in, or when
`env.step` gains per-step Python work (RAM variables and a reward, which the benchmark config
leaves out, add `read_ram` work proportional to the variables read).

## Findings

### FBNeo and savestates

Battle Garegga (`fbneo`) first recorded fine but didn't replay in a fresh process: a loaded
state went on differently depending on what the process had run before the load. Four pieces
of state outside FBNeo's savestates caused it, each found by running the same inputs live
and from a loaded state in a fresh process, comparing the frame, the RAM and the whole
savestate after every frame, and naming the first differing savestate bytes by the scan
area that wrote them (a diagnostic build logging each `BurnArea`):

| What the state left out | Symptom without it | Fix |
|---|---|---|
| The palette the Toaplan GP9001 drivers draw with, computed after each frame (CAVE's `ket` and `ddp3` too) | The first frame after a load uses the palette the process last drew: desync at frame 1 | `settle_frames = 1` (docs/games.md, "Settle frames") |
| The YM2151's render position and carried samples (`burn_ym2151.cpp`); the game's YM2151 IRQ is timed by rendered samples | The Z80 takes its IRQ at other points: attract mode desyncs in the Z80 RAM at about frame 810 | patch `0001-ym2151-scan-render-position.patch` |
| The scanline the Toaplan scanline register last returned (`toaplan.h`), which sets its new-line bit | The 68000 polls for a different number of cycles: play desyncs at frame 870, one row of the raster split off, with RAM identical | patch `0002-toaplan-scan-previous-scanline.patch` |
| The MSM6295's render position and interpolated samples (`msm6295.cpp`) | Samples end at other points, changing the status the Z80 reads: play desyncs in the Z80 RAM at about frame 3340 | patch `0003-msm6295-scan-render-position.patch` |

The patches are applied to our FBNeo build (docs/cores.md, "Patches") until upstream has
them. With them and settling, the 36004-frame recording above replays fully (as do two more
input seeds checked by hand), and `tests/test_bgaregga.py` checks attract mode and play on
the user's ROM. The costs: the state grows from 126 KB to 159 KB (almost all of it the YM2151's
carried samples), and a load takes about 245 µs instead of 6, since it runs the settle frame.

Still open: Psikyo games (`gunbird`, `s1945`) differ after a load even with 1 to 4 settle
frames, so they leave out more state, not traced yet. Other sound chips' wrappers may share
the YM2151's and MSM6295's pattern (a render position left out of the scan): a new FBNeo
game needs its own replay check before its recordings are trusted.
