"""Shared fixtures: the test core (docs/libretro.md, "Loading the test core in tests"), the
`rom` lookup every ROM test uses (docs/roms.md), the `core` lookup of tests that run a real
core (docs/testing.md, "ROM tests"), the replay and savestate checks the FBNeo ROM tests share
and the test core's game files."""

import json
import os
import struct
import subprocess
import sys
import zipfile
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import pytest

from gtharness import _native, bench, cores
from gtharness.env import GAMES_DIR, RetroEnv, core_library
from gtharness.record import Record

ROOT = Path(__file__).resolve().parent.parent
DATA_DIR = ROOT / "data"
"""The checkout's git-ignored data directory, where `gth cores fetch data` puts the cores."""
LIBRARY = {"win32": "gth_testcore.dll", "darwin": "libgth_testcore.dylib"}.get(
    sys.platform, "libgth_testcore.so"
)


@pytest.fixture(scope="session")
def testcore_path() -> Path:
    """The test core's library, built once per session before any test loads it. The same
    rule as gth_test_support::testcore_path: build with cargo and take the path from cargo's
    JSON messages, which works with any CARGO_TARGET_DIR. Cargo runs directly (not nested in
    `cargo run`), so no outer cargo holds the build lock on Windows."""
    cargo = os.environ.get("CARGO", "cargo")
    result = subprocess.run(
        [cargo, "build", "--locked", "-p", "gth-testcore", "--message-format=json"],
        cwd=ROOT,
        capture_output=True,
        encoding="utf-8",
        check=False,
    )
    assert result.returncode == 0, result.stderr
    for line in result.stdout.splitlines():
        message = json.loads(line) if line.startswith("{") else {}
        if message.get("reason") == "compiler-artifact":
            for file in message.get("filenames", []):
                if Path(file).name == LIBRARY:
                    return Path(file)
    raise AssertionError(f"cargo didn't report {LIBRARY}")


@pytest.fixture
def rom() -> Callable[[str], Path]:
    """``rom(game_id)``: the game's content file in the ROM directory (GTH_ROM_DIR or the user
    config's rom_dir), found by SHA-1. Skips the test when the directory is unset or the file
    isn't in it, so tests that need a user's ROM pass on CI, which has none."""

    def find(game_id: str) -> Path:
        config = _native.GameConfig.load(GAMES_DIR / game_id / "game.toml")
        rom_dir = _native.rom_dir()
        if rom_dir is None:
            pytest.skip(f"{game_id}: no ROM directory (set GTH_ROM_DIR, docs/roms.md)")
        path = config.find_content(rom_dir)
        if path is None:
            pytest.skip(f"{game_id}: {config.content_file} isn't in {rom_dir}")
        return path

    return find


@pytest.fixture
def core() -> Callable[..., Path]:
    """``core(game_id, data_dir=DATA_DIR)``: the game's core in ``data_dir/cores/``. Skips the
    test when it is missing or isn't the build `cores.lock` pins for this platform (a ROM test
    pins values that another build may not reproduce), so a machine with ROMs but no fetched
    core skips rather than fails."""

    def find(game_id: str, data_dir: Path = DATA_DIR) -> Path:
        name = _native.GameConfig.load(GAMES_DIR / game_id / "game.toml").core
        path = core_library(data_dir, name)
        if not path.is_file():
            pytest.skip(
                f"{game_id}: no {path.name} in {path.parent} (gth cores fetch, docs/cores.md)"
            )
        lock = _native.CoresLock.load(cores.LOCK)
        if cores.TARGET not in lock.artifacts(name):
            pytest.skip(
                f"{game_id}: cores.lock pins no {cores.TARGET} build of {name} yet: its release"
                " isn't written into the lock (docs/cores.md)"
            )
        if not lock.pins(name, cores.TARGET, path):
            pytest.skip(f"{game_id}: {path} isn't the build cores.lock pins (gth cores fetch)")
        return path

    return find


# The replay and cold-load check's run: attract mode with random shots, a savestate at
# STATE_STEP, then no button held for a while.
FRAMESKIP = 4
SHOT = 1  # flat action: no direction, the shot slot (game.toml: a, or b on gunbird and s1945)
UP_SHOT = 1 * 16 + SHOT  # flat action: up and shot (docs/actions.md)
# Steps of boot and attract mode (the PGM BIOS boot first); every game tested animates sprites
# throughout it.
STEPS = 1000
STATE_STEP = 750
FRESH_LOAD = Path(__file__).resolve().parent / "fresh_load.py"


def _no_reward(previous, current) -> float:
    """The games checked have no RAM variables yet, so no reward reads one."""
    return 0.0


@dataclass
class ColdLoad:
    """What `replay_and_cold_load` leaves to the test: the first frame after loading the state,
    settled, in a core of the test's process that ran 3000 frames first, and unsettled in a
    fresh process."""

    warm_first: np.ndarray
    fresh_first_unsettled: np.ndarray


@pytest.fixture
def replay_and_cold_load(rom, core, tmp_path: Path) -> Callable[..., ColdLoad]:
    """``replay_and_cold_load(game_id, obs=..., settle_frames=..., settled_steps=...)``: runs
    STEPS steps of the game, recorded with a check every step, saving a state at STATE_STEP, and
    checks that a savestate loaded into a core gives what the live run did whatever ran before
    the load (docs/performance.md, "FBNeo and savestates"):

    - the game's `settle_frames` is ``settle_frames``;
    - the recording replays to its end in a fresh process (fresh_load.py), with no desync;
    - an environment reset from the state shows the live run's frame at STATE_STEP, in this
      process after the run and in a fresh one;
    - a settled core's first frame after the load is the same in a fresh process as in this
      one, after 3000 other frames;
    - unless ``settled_steps`` is None, the whole savestate ``settled_steps`` steps after the
      load (no button held from STATE_STEP on) is the live run's, in this process and in the
      fresh one.

    ``obs`` is the core's own picture size, so frames compare unresized. Returns the frames the
    tests' checks of why settling is on compare. Skips without the ROM or the pinned core."""

    def check(
        game_id: str, *, obs: tuple[int, int], settle_frames: int, settled_steps: int | None
    ) -> ColdLoad:
        content = rom(game_id)
        core_path = core(game_id)
        data = core_path.parent.parent
        game_toml = GAMES_DIR / game_id / "game.toml"
        game = _native.GameConfig.load(game_toml)
        assert game.settle_frames == settle_frames
        assert settle_frames > 0, "the check compares settled loads with unsettled ones"
        assert settled_steps is None or settled_steps > 0
        held_steps = 1 if settled_steps is None else settled_steps
        env = RetroEnv.from_game(
            game_id,
            data,
            frameskip=FRAMESKIP,
            obs_size=obs,
            action_format="flat",
            reward=_no_reward,
            sticky_prob=0.0,
            max_episode_frames=None,
            noop_max=0,
            later_episodes="reload",
            maxpool=False,
            grayscale=False,
            core_path=core_path,
            record=Record(
                directory=tmp_path / "recordings",
                recorder="scripted",
                player="scripted",
                license="CC0-1.0",
                checks_every=1,
            ),
        )
        rng = np.random.default_rng(0)
        try:
            env.reset()
            for step in range(STEPS):
                if step == STATE_STEP:
                    state = env.save_state()
                if STATE_STEP <= step < STATE_STEP + held_steps:
                    observation, *_ = env.step(0)
                    if step == STATE_STEP:
                        live = observation
                    if settled_steps is not None and step == STATE_STEP + settled_steps - 1:
                        live_settled_state = env.save_state()
                else:
                    env.step(int(rng.choice([0, SHOT, UP_SHOT])))
            # In this process, after everything above: the reset from the state shows what the
            # live run did. It ends the recording and starts another.
            observation, _ = env.reset(options={"state": state})
            np.testing.assert_array_equal(observation, live)
        finally:
            env.close()
        recording = env.recordings[0]

        # And this process's first frame after loading the state, with its history, for the
        # fresh process to match, and its savestate settled_steps steps later.
        warm = _native.Core(
            core_path,
            content,
            data / "system",
            game.core_options,
            game.settle_frames,
            *obs,
            game.crop,
            game.orientation,
            "area",
            False,
            False,
        )
        try:
            warm.step(3000, 0)
            warm.unserialize(state)
            warm_first = warm.step(1, 0)
            if settled_steps is not None:
                warm.step(settled_steps * FRAMESKIP - 1, 0)
                warm_settled_state = warm.serialize()
        finally:
            warm.close()

        out = tmp_path / "fresh"
        out.mkdir()
        (out / "state").write_bytes(state)
        run = subprocess.run(
            [
                sys.executable,
                str(FRESH_LOAD),
                game_id,
                str(recording),
                str(core_path),
                str(content),
                str(out),
                str(FRAMESKIP),
                *map(str, obs),
                str(0 if settled_steps is None else settled_steps * FRAMESKIP),
                str(game_toml),
            ],
            capture_output=True,
            text=True,
        )
        # A desync raises DesyncError in the replay, naming its frame.
        assert run.returncode == 0, run.stderr
        replayed, frames = map(int, run.stdout.split())
        assert replayed == frames == (STEPS + 1) * FRAMESKIP, "the reset's frames, then the steps'"

        np.testing.assert_array_equal(np.load(out / "fresh-reset.npy"), live)
        np.testing.assert_array_equal(np.load(out / f"fresh-first-{settle_frames}.npy"), warm_first)
        if settled_steps is not None:
            assert warm_settled_state == live_settled_state, "this process's state after the load"
            fresh_settled_state = (out / "fresh-settled-state").read_bytes()
            assert fresh_settled_state == live_settled_state, (
                "the fresh process's state after the load"
            )
        return ColdLoad(warm_first, np.load(out / "fresh-first-0.npy"))

    return check


@pytest.fixture
def play_replays(rom, core, tmp_path: Path) -> Callable[[str], None]:
    """``play_replays(game_id)``: the benchmark's determinism check (docs/performance.md), which
    coins and starts a game, so its recording covers play, not only attract mode: 4000 frames
    replay in a fresh process with no desync. Skips without the ROM or the pinned core."""

    def check(game_id: str) -> None:
        content = rom(game_id)
        data = core(game_id).parent.parent
        target = bench.game_target(game_id, data, content.parent)
        result = bench.determinism(target, 4000, tmp_path, seed=0)
        assert result["desync"] is None, result["desync"]
        assert result["replayed"] == result["frames"]

    return check


# games/testcore/game.toml's files: not ROMs, so tests write them.
TESTCORE_CONTENT = b"gth-testcore content: seeds the test core's PRNG\n"
TESTCORE_BIOS = b"gth-testcore stand-in BIOS: the test core reads none\n"


@pytest.fixture
def testcore_data(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A data directory with the test core game's BIOS files, and GTH_ROM_DIR pointing at a
    ROM directory with its content (renamed, so it is found by SHA-1) and parent set."""
    roms = tmp_path / "roms" / "sub"
    roms.mkdir(parents=True)
    (roms / "renamed.gthc").write_bytes(TESTCORE_CONTENT)
    with zipfile.ZipFile(roms / "gth_testcore_parent.zip", "w", zipfile.ZIP_DEFLATED) as parent:
        parent.writestr("parent.bin", TESTCORE_BIOS)
    monkeypatch.setenv("GTH_ROM_DIR", str(tmp_path / "roms"))
    system = tmp_path / "data" / "system"
    (system / "gth").mkdir(parents=True)
    (system / "gth_testcore_bios.bin").write_bytes(TESTCORE_BIOS)
    with zipfile.ZipFile(system / "gth" / "gth_testcore_bios.zip", "w") as bios:
        bios.writestr("bios.bin", TESTCORE_BIOS)
    return tmp_path / "data"


@pytest.fixture
def zstd_raw_frame() -> Callable[[bytes], bytes]:
    """``zstd_raw_frame(data)``: ``data`` as a zstd frame, for tests that rewrite a
    recording's files (Python 3.12 has no zstd module)."""
    return _zstd_raw_frame


def _zstd_raw_frame(data: bytes) -> bytes:
    """``data`` as a zstd frame of raw (uncompressed) blocks (RFC 8878): single segment, a
    4-byte content size, no checksum."""
    frame = struct.pack("<IBI", 0xFD2FB528, 0b1010_0000, len(data))
    blocks = [data[i : i + 128 * 1024] for i in range(0, len(data), 128 * 1024)] or [b""]
    for i, block in enumerate(blocks):
        last = int(i == len(blocks) - 1)
        frame += (len(block) << 3 | last).to_bytes(3, "little") + block
    return frame
