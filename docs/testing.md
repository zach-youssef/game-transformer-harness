# Testing

Every change comes with tests. Most run on the CPU against `gth-testcore`, everywhere,
including CI; tests that need a user's ROM skip where there is none; GPU tests run on
TheaterPC only.

## Where tests go

| What | Where | Run by |
|---|---|---|
| A crate's logic | Unit tests in the crate (`#[cfg(test)]` modules, or `src/tests.rs`) | `cargo test` |
| A crate against a loaded core | `crates/<crate>/tests/testcore.rs`, loading `gth-testcore` through `gth_test_support::testcore_path()` | `cargo test` |
| `_native` bindings (`gth-py`, which has no Rust tests) | `tests/test_native_*.py` | pytest |
| The Python package: env, recording, shards, training windows, CLI, captures, game configs, benchmark, evaluation protocols, policies | `tests/test_env.py`, `test_record.py`, `test_shards.py`, `test_windows.py`, `test_cli.py`, `test_capture.py`, `test_games.py`, `test_bench.py`, `test_protocol.py`, `test_policies.py` | pytest |
| A game on its real core and the user's ROM (skips without them) | `tests/test_<game-id>.py`, e.g. `test_gunhed.py` | pytest |

Shared pytest fixtures are in `tests/conftest.py`: `testcore_path` (the built test core), `rom`
and `core` (below), `testcore_data` (the files `games/testcore/game.toml` names, written to a
temporary directory), `zstd_raw_frame` (bytes as a zstd frame, for rewriting a recording's
files), and the checks the FBNeo ROM tests share: `replay_and_cold_load` (a recording replays
in a fresh process and a savestate loads alike whatever the process ran before, in the fresh
process `tests/fresh_load.py` runs too) and `play_replays` (the benchmark's determinism check
on a game). pytest imports test files with `--import-mode=importlib`, so tests can't import
each other or a helper module beside them: shared test code goes in a `conftest.py` fixture.
The test core is deterministic, so tests assert exact frames, RAM values and hashes rather than
ranges (docs/libretro.md, "Test core").

- **One core per process** (architecture.md): pytest runs in one process, so every test that
  loads a core, through `RetroEnv`, `_native.Core` or `_native.Replayer`, closes it, even when
  the test fails (a fixture or `try`/`finally`). A leaked core makes every later load fail
  with "another core is loaded". Rust test binaries are separate processes, but the tests in
  one binary run on parallel threads of one process, so each core-loading test first takes
  the file's `SERIAL` mutex (`let _serial = serial();`, as in `crates/gth-core/tests/testcore.rs`)
  and drops its `Core` before releasing it. `gtharness.shards.export_shards` loads its cores
  in spawned processes, so it holds none in pytest's; its test hook
  `GTHARNESS_TEST_EXIT_IN_EXPORT` (a recording name) makes that recording's worker die, as a
  crashing core would.
- Don't rebuild the test core while a test has it loaded (Windows can't overwrite a loaded
  DLL); docs/libretro.md, "Loading the test core in tests".
- `maturin develop` builds debug, so pytest timings are debug numbers. Measure speed after
  `maturin develop --release`, and don't assert timings in tests.

## CPU tests in CI

CI (`.github/workflows/ci.yml`, docs/development.md) runs `cargo fmt --check`, clippy with
`-D warnings`, `cargo test`, `ruff check`, `ruff format --check` and pytest on Linux, macOS
(arm64) and Windows, for every pushed branch. CI has no ROMs, no real cores and no GPU: a test
that needs one of those skips there (ROMs) or doesn't run there (GPU), and whatever it covers
that the test core can reproduce gets a test-core test too, so CI still checks it.

## ROM tests

A test that needs a user's ROM is a pytest test taking the `rom` fixture (`cargo test` can't
skip with a reason, so ROM tests stay out of Rust). `rom(game_id)` returns the path of the
game's content, found by SHA-1 (docs/roms.md, "Finding files") in the ROM directory, which is
`GTH_ROM_DIR` or else `rom_dir` in the user config file. It skips the test with a reason when:

- no ROM directory is set (`<game>: no ROM directory ...`), as on CI;
- no file in it has the content's expected SHA-1 (`<game>: <file> isn't in <dir>`).

A skip is quiet: without `GTH_ROM_DIR` (or `rom_dir`) a ROM test run reports only passes and
`s`s, having checked nothing. Run ROM tests with `-rs` and read that nothing skipped.

A ROM directory that is set but doesn't exist is a misconfiguration, not a missing ROM: the
fixture raises `ConfigError` rather than skipping, so the mistake is seen. Never add a ROM,
BIOS file or anything derived from one to `tests/` (docs/roms.md, "Rules").

A ROM test that runs the real core also takes the `core` fixture: `core(game_id)` returns
the game's core library in the checkout's git-ignored `data/cores/` (where
`uv run gth cores fetch data` puts it, docs/cores.md), whose parent is then the data
directory for `RetroEnv.from_game`. It skips with a reason when the library is missing or
isn't the build `cores.lock` pins for this platform (including when the lock pins none yet,
between a pin change and its release), so a machine with ROMs but no fetched core
skips rather than fails, and values a test pins (scores, frames) are always checked on the
build they came from. To run one against a local build before its release, put the build in
`data/cores/` and its hash in your copy of the lock for that run only (never commit it).
`core(game_id, data_dir)` looks in another data directory. No game with BIOS files has a ROM
test yet: `RetroEnv.from_game` raises naming any missing from `data/system/`, and the first
such test adds a skip for them.
`tests/test_gunhed.py` is the example (docs/games.md, "Adding a game"):

```sh
GTH_ROM_DIR=~/roms uv run pytest tests/test_gunhed.py -rs   # -rs prints why a test skipped
```

## GPU tests

The harness itself emulates on the CPU and has no GPU tests yet. Work that needs a GPU
(policy inference, the NitroGen bridge, training) is tested on TheaterPC, the Windows machine
with the GPU, and those tests run there one at a time: the GPU is shared, and two runs at
once make each other's timings and memory use meaningless. The first GPU test registers a
`gpu` pytest marker in `pyproject.toml` (pytest runs with `--strict-markers`) and makes CI
deselect it.

## Per-commit checks

Every commit passes the checks CI runs, on its own, so any commit of a branch can be
bisected or merged. CI checks only a branch's head, so before pushing a branch with several
commits, run the CI commands (docs/development.md, "Commands") on each one, for example with
`git rebase --exec '<script running them>' origin/main`.

Judge a run by its exit code, not by its filtered output: `cargo test | grep "test result"`
exits 0 whatever the tests did. Run a check unpiped (or with `set -o pipefail`) when its
status matters. A panic inside a core's callback (the test core's, or a host callback the test
core calls) can't unwind across the C boundary, so it aborts the whole test binary: no `test
result` line for it and none of its later tests run. When reading `cargo test`'s output, look
for `FAILED` and `panicked` as well as the summary lines. Treat any warning as a failure, but
only clippy (`-D warnings`) and ruff enforce that: nothing makes pytest fail on a warning yet,
so read pytest's output too.

## Known gaps

- `Core::reset`'s fallback to `retro_reset` (docs/libretro.md, "Host") has no test: it needs
  a test core option that makes the core refuse a savestate at load time.
