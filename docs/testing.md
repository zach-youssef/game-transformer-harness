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
| The Python package: env, recording, shards, CLI, game configs | `tests/test_env.py`, `test_record.py`, `test_shards.py`, `test_cli.py`, `test_games.py` | pytest |

Shared pytest fixtures are in `tests/conftest.py`: `testcore_path` (the built test core),
`rom` (below) and `testcore_data` (the files `games/testcore/game.toml` names, written to a
temporary directory). The test core is deterministic, so tests assert exact frames, RAM
values and hashes rather than ranges (docs/libretro.md, "Test core").

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

A ROM directory that is set but doesn't exist is a misconfiguration, not a missing ROM: the
fixture raises `ConfigError` rather than skipping, so the mistake is seen. Never add a ROM,
BIOS file or anything derived from one to `tests/` (docs/roms.md, "Rules").

No test uses a real core yet. The first one adds a fixture like `rom` that finds the core
(pinned in `cores.lock`, docs/cores.md, coming with P1) and the data directory with the game's
BIOS files, and skips with a reason when either is missing, so a machine with ROMs but no
built core skips rather than fails.

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
status matters. Treat any warning as a failure, but only clippy (`-D warnings`) and ruff
enforce that: nothing makes pytest fail on a warning yet, so read pytest's output too.

## Known gaps

- `Core::reset`'s fallback to `retro_reset` (docs/libretro.md, "Host") has no test: it needs
  a test core option that makes the core refuse a savestate at load time.
