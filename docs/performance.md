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

None yet: run the benchmark and add a section per machine.

## Decisions

### P5: where batching lives

The trigger: if the Python overhead per step is more than 10% of the step's time at
frameskip 1, P5 moves batching into Rust (a shared-memory ring); otherwise P5 stays a
multiprocessing vector env with shared-memory observations.
