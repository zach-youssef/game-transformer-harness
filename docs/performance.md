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

Machine: Apple M4, 10 CPUs (4 performance, 6 efficiency), macOS 26.6.2; harness 7f81c13
(release), with every core from the release `cores-afa730086cb24842` (`gth cores fetch`).

| Target | Core | Version | Pinned build |
|---|---|---|---|
| gunhed | `mednafen_pce_fast` | `v1.31.0.0 2de1ce1` | yes |
| zanac | `fceumm` | `(SVN) e0f3086` | yes |
| lightening-force | `genesis_plus_gx` | `v1.7.4 b9c2316` | yes |
| bgaregga | `fbneo` | `v1.0.0.03 261110 GIT0630faa` | yes |

Single process, 256x256 observations: frames/s and the time of one step in µs.

| Target | Frameskip | Frames/s | Env step | Native step | Emulate | Render | Python | Python share |
|---|---|---|---|---|---|---|---|---|
| gunhed | 1 | 4060 | 246 | 245 | 146 | 99 | 0.9 | 0.4% |
| gunhed | 2 | 4944 | 405 | 404 | 307 | 97 | 1.0 | 0.3% |
| gunhed | 4 | 5685 | 704 | 703 | 607 | 96 | 1.2 | 0.2% |
| zanac | 1 | 5957 | 168 | 168 | 127 | 41 | 0.9 | 0.5% |
| zanac | 2 | 6756 | 296 | 295 | 254 | 42 | 0.9 | 0.3% |
| zanac | 4 | 7377 | 542 | 545 | 505 | 40 | 0.9 | 0.2% |
| lightening-force | 1 | 1551 | 645 | 645 | 237 | 408 | 1.3 | 0.2% |
| lightening-force | 2 | 2287 | 874 | 870 | 467 | 402 | 1.4 | 0.2% |
| lightening-force | 4 | 2953 | 1355 | 1354 | 950 | 403 | 1.5 | 0.1% |
| bgaregga | 1 | 1725 | 580 | 576 | 247 | 329 | 0.9 | 0.2% |
| bgaregga | 2 | 2450 | 816 | 820 | 483 | 337 | 0.9 | 0.1% |
| bgaregga | 4 | 2839 | 1409 | 1413 | 1084 | 329 | 1.1 | 0.1% |

Processes at once, 256x256 observations: total frames/s (and per process).

| Target | Frameskip | N = 1 | N = 2 | N = 4 | N = 9 |
|---|---|---|---|---|---|
| gunhed | 1 | 3842 (3842) | 7948 (3974) | 14510 (3628) | 22367 (2485) |
| gunhed | 2 | 5384 (5384) | 10280 (5140) | 19617 (4904) | 29177 (3242) |
| gunhed | 4 | 6427 (6427) | 12598 (6299) | 23408 (5852) | 34709 (3857) |
| zanac | 1 | 5981 (5981) | 11679 (5840) | 21371 (5343) | 31988 (3554) |
| zanac | 2 | 6737 (6737) | 13147 (6573) | 24839 (6210) | 36971 (4108) |
| zanac | 4 | 7170 (7170) | 14360 (7180) | 27112 (6778) | 40012 (4446) |
| lightening-force | 1 | 1643 (1643) | 3245 (1623) | 6016 (1504) | 9845 (1094) |
| lightening-force | 2 | 2345 (2345) | 4606 (2303) | 8925 (2231) | 13962 (1551) |
| lightening-force | 4 | 2944 (2944) | 5799 (2899) | 11384 (2846) | 17945 (1994) |
| bgaregga | 1 | 1594 (1594) | 3127 (1564) | 5644 (1411) | 9016 (1002) |
| bgaregga | 2 | 2128 (2128) | 4179 (2090) | 7699 (1925) | 12403 (1378) |
| bgaregga | 4 | 2537 (2537) | 4993 (2496) | 9414 (2354) | 14955 (1662) |

| Target | Savestate bytes | Save µs | Load µs | Reload matches |
|---|---|---|---|---|
| gunhed | 80586 | 10 | 73 | yes, 600 frames |
| zanac | 21976 | 2 | 9 | yes, 600 frames |
| lightening-force | 1036288 | 35 | 14 | yes, 600 frames |
| bgaregga | 158836 | 16 | 244 | yes, 600 frames |

| Target | Frames recorded | Replayed | Desync | Record s | Replay s |
|---|---|---|---|---|---|
| gunhed | 36004 | 36004 | none | 7.5 | 6.3 |
| zanac | 36004 | 36004 | none | 7.9 | 7.3 |
| lightening-force | 36004 | 36004 | none | 12.6 | 11.3 |
| bgaregga | 36004 | 36004 | none | 17.0 | 16.2 |

The numbers are from one run with no builds running (`pgrep -f '[c]argo|[r]ustc'` logged
every second). An earlier run the same day, with a local build of the same FBNeo source
before its release, agreed within about 2% for single-process frames/s (Battle Garegga
included), except Gunhed, about 6% slower then; its 9-process totals were 2-5% lower for
every target. Save and load times vary by a few µs between runs. What they show:

- Rendering the 256x256 observation costs a fixed amount per step (40-410 µs, depending on
  the core's frame size and pixel format), while emulating scales with the frames run, so
  frameskip raises frames/s most where rendering is a large share of the step: from
  frameskip 1 to 4, Lightening Force gains 1.9x, Battle Garegga 1.6x, Gunhed 1.4x and Zanac
  1.2x.
- Processes scale nearly linearly up to the four performance cores (each process keeps
  88-97% of its single-process rate at N = 4). From 4 to 9 processes the total rises by
  48-64%: each of the five extra processes, on efficiency cores, adds 38-51% of a
  performance core's rate. The parallel runs play on from the reset without restoring a
  state, so their scenes differ from the single-process table's (Gunhed at frameskip 4 runs
  6427 frames/s there against 5685 here).
- Savestates are small (22 KB to 1 MB) and take 2-73 µs to save or load, so per-step
  checkpoints for resets or branching are cheap. Battle Garegga's load takes 244 µs because
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
step at frameskip 1 (and less at higher frameskip); even Zanac, the cheapest step at 168 µs,
would need 17 µs of overhead to reach 10%. Recheck when TheaterPC's numbers are in, or when
`env.step` gains per-step Python work (RAM variables and a reward, which the benchmark config
leaves out, add `read_ram` work proportional to the variables read).

## Findings

### FBNeo and savestates

Battle Garegga (`fbneo`) replays in a fresh process on the pinned build: FBNeo's savestates
left out four pieces of state, so a loaded state went on differently depending on what the
process had run before the load. Settling after a load and three commits on our FBNeo fork
deal with them. Each was found by running the same inputs live and from a loaded state in a
fresh process, comparing the frame, the RAM and the whole savestate after every frame, and
naming the first differing savestate bytes by the scan area that wrote them (a diagnostic
build logging each `BurnArea`):

| What the state left out | Symptom without it | Fix |
|---|---|---|
| The palette the Toaplan GP9001 drivers draw with, computed after each frame (CAVE's `ket` and `ddp3` too) | The first frame after a load uses the palette the process last drew: desync at frame 1 | `settle_frames = 1` (docs/games.md, "Settle frames") |
| The YM2151's render position and carried samples (`burn_ym2151.cpp`); the game's YM2151 IRQ is timed by rendered samples | The Z80 takes its IRQ at other points: attract mode desyncs in the Z80 RAM at about frame 810 | fork commit `ddb1e8c` (`burn_ym2151`) |
| The scanline the Toaplan scanline register last returned (`toaplan.h`), which sets its new-line bit | The 68000 polls for a different number of cycles: play desyncs at frame 870, one row of the raster split off, with RAM identical | fork commit `904777f` (`toaplan`) |
| The MSM6295's render position and interpolated samples (`msm6295.cpp`) | Samples end at other points, changing the status the Z80 reads: play desyncs in the Z80 RAM at about frame 3340 | fork commit `9fbf243` (`msm6295`) |

The pinned FBNeo build is our fork's `gth` branch with these commits (docs/cores.md,
"Forks"). They were first released as patches applied to upstream's `a836e52` at build time
(release `cores-afa730086cb24842`, the same source). On that build, with settling, the
36004-frame recording above replays fully (as did two more input seeds checked by hand on a
local build of the same changes), and `tests/test_bgaregga.py` checks that attract mode and
play replay and that loaded states match on the user's ROM. The costs: the state grows from
126 KB to 159 KB (almost all of it the YM2151's carried samples), and a load takes about
244 µs instead of 6, since it runs the settle frame.

Psikyo games (`gunbird`, `s1945`) left out four more, found the same way and fixed by four
more fork commits:

| What the state left out | Symptom without it | Fix |
|---|---|---|
| The sprite lists the Psikyo driver copies out of sprite RAM at each vblank and draws two frames later (`psikyo_sprite.cpp`, double-buffered sprites) | The first two frames after a load draw the sprites of whatever the process last ran (none in a fresh process); settling can't fix it, since the settle frames buffer later sprite RAM than the saved run drew, so 1 to 4 settle frames still gave different first frames; RAM identical | fork commit `b061823` (`psikyo`) |
| The YM2610's resampling position and carried samples (`burn_ym2610.cpp`; the render positions were saved, the fraction and the samples weren't) | The frame after a load ends at another sample count, and the chip's slot phases drift from the saved run's from the first frame on (Gunbird: `YM2610.CH*.SLOT*:phasecount` differs in the state one frame after a load); audio differs, and the sound CPU could see it through the ADPCM flags | fork commit `ad94ca3` (`burn_ym2610`) |
| The YMF278B's render position and carried samples (`burn_ymf278b.cpp`), which its scan zeroed on every load | The chip renders a full frame again from position 0, so its slots run ahead of the saved run's for good (Strikers 1945: `m_slots` differs one frame after a load); audio only, since its sound CPU state is fully scanned | fork commit `e85680d` (`burn_ymf278b`) |
| The YM2610 FM core's LFO and envelope counters (`fm.c`: `lfo_cnt`, `lfo_inc`, `eg_cnt`, `eg_timer`; the YM2203 and YM2612 lists have them) | With the render position saved, slot volumes and LFO phases still stepped from another counter after a load (Gunbird: `volume`, `vol_out`, `phasecount` differ one frame after it) | fork commit `f824e12` (`fm.c`) |

With them, a state loaded in a fresh process continues byte for byte as the run that saved it
(the whole savestate was compared frame by frame for 120 frames after loads of 12 checkpoints
per game, and frames and RAM for 2000 more), and both games' 36004-frame benchmark recordings
of play replay in a fresh process with `settle_frames = 1` (`tests/test_psikyo.py` checks
attract mode and play on the user's ROMs). The settle frame is still needed, for a reason
outside the driver: FBNeo's libretro port dupes the first frame a process runs
(`retro_get_system_av_info` flags its video buffer for reallocation, and the first
`retro_run` passes no frame to `video_cb`), so a fresh process that loads a state and draws
at once shows the host's cleared frame. The settle frame absorbs it, for every FBNeo game.
The costs: Gunbird's state grows from 184 KB to 299 KB (64 KB of sprite lists, 48 KB of
YM2610 carried samples), Strikers 1945's from 183 KB to 265 KB (the sprite lists and 16 KB of
YMF278B samples).

Other sound chips' wrappers may share the pattern (a render position left out of the scan):
`burn_ym2203.cpp`, `burn_ym2608.cpp`, `burn_ym2612.cpp` and the YM2608 FM core's counters
haven't been checked, so a new FBNeo game needs its own replay check before its recordings
are trusted. The method that finds these is the whole-savestate comparison one frame after a
fresh-process load: both Psikyo sound bugs left the RAM identical for 6000 frames.
