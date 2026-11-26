"""RetroEnv against the test core: inputs, frameskip, savestates, determinism, gymnasium API.

The test core draws one cell per joypad button (bright red when held), writes the frame
number into the background's green channel, moves a white square with the d-pad and keeps
a score in its RAM (docs/libretro.md, "Test core").

Every env is closed even when a test fails (the `env` fixture or `retro_env`): one core per
process, so a leaked one would fail every later test with "another core is loaded".
"""

import contextlib
import hashlib
import time
from collections.abc import Iterator
from pathlib import Path

import numpy as np
import pytest
from gymnasium.utils.env_checker import check_env

from gtharness import _native
from gtharness.env import RetroEnv
from gtharness.errors import CoreLoadError, HarnessError

# RETRO_DEVICE_ID_JOYPAD_* as bits.
B, START, RIGHT, DOWN, A = 1 << 0, 1 << 3, 1 << 7, 1 << 5, 1 << 8
RETRO_MEMORY_SYSTEM_RAM = 2
WIDTH, HEIGHT = 320, 240


def press(mask: int) -> np.ndarray:
    """The MultiBinary(16) action holding the buttons in `mask`."""
    return np.array([(mask >> i) & 1 for i in range(16)], dtype=np.int8)


def make(testcore_path: Path, tmp_path: Path, **kwargs) -> RetroEnv:
    options = {"frameskip": 1, "obs_size": (HEIGHT, WIDTH), "rotate": 0} | kwargs
    return RetroEnv(testcore_path, None, tmp_path, **options)


@contextlib.contextmanager
def retro_env(testcore_path: Path, tmp_path: Path, **kwargs) -> Iterator[RetroEnv]:
    env = make(testcore_path, tmp_path, **kwargs)
    try:
        yield env
    finally:
        env.close()


@pytest.fixture
def env(testcore_path: Path, tmp_path: Path) -> Iterator[RetroEnv]:
    """The full-size frame, so pixels map one to one."""
    with retro_env(testcore_path, tmp_path) as env:
        yield env


def frame_number(obs: np.ndarray) -> int:
    """The background's green channel, below the button cells."""
    return int(obs[100, 0, 1])


def ram(env: RetroEnv) -> bytes:
    memory = env.memory(RETRO_MEMORY_SYSTEM_RAM)
    assert memory is not None
    return memory


def test_reset_runs_frameskip_frames_and_steps_show_held_buttons(env: RetroEnv):
    obs, info = env.reset(seed=0)
    assert obs.shape == (HEIGHT, WIDTH, 3) and obs.dtype == np.uint8
    assert frame_number(obs) == 1, "reset ran one frame with no buttons"
    assert not (obs == [255, 0, 0]).all(axis=2).any(), "no button held"
    assert info == {"episode_frame": 1}
    obs, reward, terminated, truncated, info = env.step(press(A | START))
    assert (reward, terminated, truncated, info) == (0.0, False, False, {"episode_frame": 2})
    cell = lambda button: obs[0, button * 20 + 10]  # noqa: E731
    assert list(cell(8)) == [255, 0x80, 0x80], "A held"
    assert list(cell(3)) == [255, 0x30, 0x30], "START held"
    assert list(cell(0)) == [0, 2, 0], "B not held: background of frame 2"
    assert ram(env)[:4] == bytes([0, 0, 0, 1])
    other = env.step(press(B))[0]
    assert list(other[0, 10]) == [255, 0, 0] and list(other[0, 8 * 20 + 10]) == [0, 3, 0]


def test_actions_are_validated(env: RetroEnv):
    env.reset()
    assert env.action_space.shape == (16,)
    for bad in (np.zeros(15), np.full(16, 2), A, [0.5] * 16):
        with pytest.raises(ValueError, match="action"):
            env.step(bad)


def test_frameskip_advances_k_frames_and_is_settable(env: RetroEnv):
    env.frameskip = 4
    obs, info = env.reset()
    assert info["episode_frame"] == 4 and frame_number(obs) == 4
    obs, *_, info = env.step(press(RIGHT | DOWN))
    assert info["episode_frame"] == 8 and frame_number(obs) == 8
    white = np.argwhere((obs == 255).all(axis=2))
    assert tuple(white.min(axis=0)) == (112 + 8, 152 + 8), "2 pixels per frame"
    env.frameskip = 3
    assert env.step(press(A))[4]["episode_frame"] == 11
    assert ram(env)[:4] == bytes([0, 0, 0, 3]), "A scores once per frame held"
    for bad in (0, -1, 1.5, True):
        with pytest.raises(ValueError, match="frameskip"):
            env.frameskip = bad
    assert env.frameskip == 3


def test_reset_starts_every_episode_identically(env: RetroEnv):
    assert env.resets_to_load_state
    first = env.reset()[0]
    for _ in range(5):
        env.step(press(A | RIGHT))
    again = env.reset()[0]
    assert np.array_equal(first, again)
    assert ram(env)[:4] == bytes(4), "the score is back to 0"


def test_savestate_round_trip_reproduces_frames_and_ram(env: RetroEnv):
    env.reset()
    for buttons in (A, RIGHT, A | DOWN):
        env.step(press(buttons))
    state = env.save_state()
    inputs = [A, RIGHT | DOWN, 0, START, A | B]

    def play() -> list[tuple[bytes, bytes]]:
        return [(env.step(press(b))[0].tobytes(), ram(env)) for b in inputs]

    obs, info = env.reset(options={"state": state})
    first = play()
    assert frame_number(obs) == 5, "the state was at frame 4"
    assert info == {"episode_frame": 1}
    assert np.array_equal(env.reset(options={"state": state})[0], obs)
    assert play() == first
    with pytest.raises(HarnessError, match="savestate"):
        env.reset(options={"state": state[:10]})


def test_deterministic_over_1000_frames(env: RetroEnv):
    env.reset()
    env.step(press(RIGHT))
    state = env.save_state()

    def run() -> str:
        digest = hashlib.sha256()
        digest.update(env.reset(options={"state": state})[0].tobytes())
        for i in range(1000):
            obs = env.step(press((i * 2_654_435_761 >> 16) & 0xFFFF))[0]
            digest.update(obs.tobytes())
            digest.update(ram(env))
        return digest.hexdigest()

    assert run() == run()


def test_second_core_in_a_process_fails_cleanly(env: RetroEnv, testcore_path: Path, tmp_path):
    with pytest.raises(CoreLoadError, match="another core is loaded"):
        make(testcore_path, tmp_path)
    env.reset()
    env.step(press(A))  # the first is untouched
    env.close()
    env.close()  # idempotent
    with pytest.raises(ValueError, match="closed"):
        env.step(press(A))
    with retro_env(testcore_path, tmp_path) as second:
        assert second.reset()[1] == {"episode_frame": 1}


def test_resize_rotate_and_crop(testcore_path: Path, tmp_path: Path):
    with retro_env(testcore_path, tmp_path) as full:
        full.reset()
        frame = full.step(press(A))[0]
    # The A cell is 20 pixels wide at x = 160..180, y = 0..20.
    with retro_env(testcore_path, tmp_path, obs_size=(20, 20), crop=(160, 0, 20, 20)) as crop:
        crop.reset()
        assert np.array_equal(crop.step(press(A))[0], frame[:20, 160:180])
    with retro_env(testcore_path, tmp_path, obs_size=(WIDTH, HEIGHT), rotate=90) as tate:
        tate.reset()
        assert np.array_equal(tate.step(press(A))[0], np.rot90(frame, k=-1))
    with retro_env(testcore_path, tmp_path, obs_size=(84, 84), interpolation="bilinear") as small:
        assert small.reset()[0].shape == (84, 84, 3)
        assert small.observation_space.shape == (84, 84, 3)
    for bad in ({"rotate": 180}, {"interpolation": "cubic"}, {"obs_size": (0, 4)}):
        with pytest.raises((ValueError, HarnessError)), retro_env(testcore_path, tmp_path, **bad):
            pass
    with (
        retro_env(testcore_path, tmp_path, crop=(300, 0, 40, 10)) as bad_crop,
        pytest.raises(HarnessError, match="crop"),
    ):
        bad_crop.reset()


def test_gymnasium_check_env(testcore_path: Path, tmp_path: Path):
    with retro_env(testcore_path, tmp_path, obs_size=(64, 64), frameskip=2) as env:
        check_env(env, skip_render_check=True)


def test_native_core_info(testcore_path: Path, tmp_path: Path):
    core = _native.Core(testcore_path, None, tmp_path, {}, 8, 8, None, 0, "area")
    try:
        assert core.system_info()["library_name"] == "gth-testcore"
        av = core.av_info()
        assert (av["base_width"], av["base_height"], av["fps"]) == (320, 240, 60)
        assert core.memory(0) is None, "no save RAM"
        assert core.resets_to_load_state
        assert not core.closed
    finally:
        core.close()
    assert core.closed


def test_steps_per_second(testcore_path: Path, tmp_path: Path):
    """Throughput at the default observation size (recorded, not asserted)."""
    actions = [press(i) for i in range(500)]
    with retro_env(testcore_path, tmp_path, obs_size=(256, 256), frameskip=4) as env:
        env.reset()
        start = time.perf_counter()
        for action in actions:
            env.step(action)
        elapsed = time.perf_counter() - start
    profile = _native.build_info()["profile"]
    print(
        f"\nRetroEnv on gth-testcore: {len(actions) / elapsed:.0f} steps/s"
        f" ({profile} build, frameskip 4, 256x256)"
    )
