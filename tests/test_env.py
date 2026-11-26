"""RetroEnv against the test core: actions, frameskip, savestates, determinism, gymnasium API.

The test core draws one cell per joypad button (bright red when held), writes the frame
number into the background's green channel, moves a white square with the d-pad and keeps
a score in its RAM (docs/libretro.md, "Test core"). These tests drive it through small
inline game configs; tests/test_games.py covers games/testcore/game.toml and `from_game`.

Every env is closed even when a test fails (the `env` fixture or `retro_env`): one core per
process, so a leaked one would fail every later test with "another core is loaded".
"""

import contextlib
import dataclasses
import functools
import hashlib
import time
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import numpy as np
import pytest
from gymnasium.utils.env_checker import check_env

from gtharness import _native
from gtharness.env import RetroEnv
from gtharness.errors import ConfigError, CoreLoadError, HarnessError
from gtharness.rewards import score_delta

# Directions (docs/actions.md).
NEUTRAL, RIGHT, DOWN_RIGHT = 0, 3, 4
# Button cells: RETRO_DEVICE_ID_JOYPAD_* numbers.
CELL_B, CELL_A = 0, 8
RETRO_MEMORY_SYSTEM_RAM = 2
WIDTH, HEIGHT = 320, 240


def config(
    orientation: int = 0, crop: str = "", ram: bool = True, extra_ram: str = ""
) -> _native.GameConfig:
    """The test core with A as shot and B as bomb (aux1 and aux2 unmapped), a one-frame reset
    script and its score."""
    text = f"""
core = "gth_testcore"
orientation = {orientation}
{crop}
content = {{ file = "none.gthc", size = 0, sha1 = "{"0" * 40}", crc32 = "00000000" }}
buttons = {{ a = "shot", b = "bomb" }}
reset = {{ script = [{{ press = [], frames = 1 }}] }}
episode = {{ terminate = "{"score >= 1000" if ram else "0"}" }}
"""
    if ram:
        text += '[ram]\nscore = { memory = "system_ram", offset = 0, size = 4, encoding = "bcd" }\n'
        text += extra_ram
    return _native.GameConfig.parse(text, "test config")


def act(
    direction: int = NEUTRAL, shot: int = 0, bomb: int = 0, aux1: int = 0, aux2: int = 0
) -> dict[str, Any]:
    return {"direction": direction, "buttons": np.array([shot, bomb, aux1, aux2], dtype=np.int8)}


def make(testcore_path: Path, tmp_path: Path, game=None, **kwargs) -> RetroEnv:
    options = {
        "core_path": testcore_path,
        "content_path": None,
        "system_dir": tmp_path,
        "frameskip": 1,
        "obs_size": (HEIGHT, WIDTH),
        "action_format": "dict",
        "reward": score_delta,
        "reset_state": None,
    } | kwargs
    return RetroEnv(game or config(), **options)


@contextlib.contextmanager
def retro_env(testcore_path: Path, tmp_path: Path, game=None, **kwargs) -> Iterator[RetroEnv]:
    env = make(testcore_path, tmp_path, game, **kwargs)
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


def cell(obs: np.ndarray, button: int) -> list[int]:
    return list(obs[0, button * 20 + 10])


def test_reset_runs_the_script_and_frameskip_frames_and_steps_hold_slots(env: RetroEnv):
    obs, info = env.reset(seed=0)
    assert obs.shape == (HEIGHT, WIDTH, 3) and obs.dtype == np.uint8
    assert frame_number(obs) == 2, "a one-frame script, then one frame with no buttons"
    assert not (obs == [255, 0, 0]).all(axis=2).any(), "no button held"
    assert info == {"episode_frame": 2, "ram": {"score": 0}}
    obs, reward, terminated, truncated, info = env.step(act(shot=1))
    assert (reward, terminated, truncated) == (1.0, False, False)
    assert info == {"episode_frame": 3, "ram": {"score": 1}}
    assert cell(obs, CELL_A) == [255, 0x80, 0x80], "shot holds A"
    assert cell(obs, CELL_B) == [0, 3, 0], "B not held: background of frame 3"
    assert ram(env)[:4] == bytes([0, 0, 0, 1])
    other, reward, *_ = env.step(act(bomb=1))
    assert reward == 0.0
    assert cell(other, CELL_B) == [255, 0, 0] and cell(other, CELL_A) == [0, 4, 0]
    unmapped, *_ = env.step(act(aux1=1, aux2=1))
    assert not (unmapped == [255, 0, 0]).all(axis=2).any(), "aux1 and aux2 press nothing"


def test_action_spaces(testcore_path: Path, tmp_path: Path):
    with retro_env(testcore_path, tmp_path) as env:
        env.reset()
        assert env.action_space["direction"].n == 9
        assert env.action_space["buttons"].n == 4, "every game has the four slots"
        bad_actions = (
            {"direction": 0, "buttons": [0, 0]},
            {"direction": 0, "buttons": [2, 0, 0, 0]},
            {"direction": 9, "buttons": [0, 0, 0, 0]},
            {"direction": -1, "buttons": [0, 0, 0, 0]},
            {"direction": 1.5, "buttons": [0, 0, 0, 0]},
            {"direction": True, "buttons": [0, 0, 0, 0]},
            {"buttons": [0, 0, 0, 0]},
            3,
        )
        for bad in bad_actions:
            with pytest.raises(ValueError, match="action"):
                env.step(bad)
    with retro_env(testcore_path, tmp_path, action_format="flat") as flat:
        flat.reset()
        assert flat.action_space.n == 144
        for bad in (-1, 144, 1.5, True, act()):
            with pytest.raises(ValueError, match="action"):
                flat.step(bad)


def test_flat_and_dict_actions_press_the_same_buttons(testcore_path: Path, tmp_path: Path):
    game = config()
    actions = [game.decode(flat) for flat in range(game.n_actions)]

    def play(action_format: str, to_action) -> list[bytes]:
        with retro_env(testcore_path, tmp_path, action_format=action_format) as env:
            env.reset()
            return [env.step(to_action(*a))[0].tobytes() for a in actions]

    def dict_action(direction: int, buttons: int) -> dict[str, Any]:
        return act(direction, *(buttons >> i & 1 for i in range(4)))

    assert play("flat", game.encode) == play("dict", dict_action)


def test_constructor_checks_its_choices(testcore_path: Path, tmp_path: Path):
    with pytest.raises(ValueError, match="action_format"):
        make(testcore_path, tmp_path, action_format="multibinary")
    with pytest.raises(ValueError, match="reset_state"):
        make(testcore_path, tmp_path, reset_state=b"state")
    with pytest.raises(ConfigError, match="score_delta needs the RAM variables \\['score'\\]"):
        make(testcore_path, tmp_path, config(ram=False))
    with pytest.raises(ConfigError, match=r"score_delta needs the RAM variables \['score'\]"):
        make(testcore_path, tmp_path, config(ram=False), reward=functools.partial(score_delta))
    with retro_env(testcore_path, tmp_path, config(ram=False), reward=lambda p, c: 0.5) as env:
        env.reset()
        assert env.step(act())[1] == 0.5, "any callable"

    @dataclasses.dataclass
    class Unhashable:
        def __call__(self, previous, current) -> float:
            return 0.0

    with retro_env(testcore_path, tmp_path, reward=Unhashable()) as env:
        env.reset()


def test_ram_variables_are_checked_when_the_env_is_made(testcore_path: Path, tmp_path: Path):
    past_end = 'past_end = { memory = "system_ram", offset = 254, size = 4, encoding = "le" }\n'
    with pytest.raises(HarnessError, match=r"RAM variable past_end: .* past the end"):
        make(testcore_path, tmp_path, config(extra_ram=past_end))
    # The PRNG output, seeded by the content, isn't BCD: a read fails only mid-episode.
    garbage = 'prng = { memory = "system_ram", offset = 4, size = 4, encoding = "bcd" }\n'
    content = tmp_path / "seed.gthc"
    content.write_bytes(b"any bytes seed the PRNG")
    with (
        retro_env(testcore_path, tmp_path, config(extra_ram=garbage), content_path=content) as env,
        pytest.raises(HarnessError, match=r"RAM variable prng: byte .* isn't packed BCD"),
    ):
        env.reset()
        for _ in range(20):
            env.step(act())


def test_frameskip_advances_k_frames_and_is_settable(env: RetroEnv):
    env.frameskip = 4
    obs, info = env.reset()
    assert info["episode_frame"] == 5 and frame_number(obs) == 5
    obs, *_, info = env.step(act(DOWN_RIGHT))
    assert info["episode_frame"] == 9 and frame_number(obs) == 9
    white = np.argwhere((obs == 255).all(axis=2))
    assert tuple(white.min(axis=0)) == (112 + 8, 152 + 8), "2 pixels per frame"
    env.frameskip = 3
    _, reward, _, _, info = env.step(act(shot=1))
    assert info["episode_frame"] == 12 and reward == 3.0
    assert ram(env)[:4] == bytes([0, 0, 0, 3]), "A scores once per frame held"
    for bad in (0, -1, 1.5, True):
        with pytest.raises(ValueError, match="frameskip"):
            env.frameskip = bad
    assert env.frameskip == 3


def test_reset_starts_every_episode_identically(env: RetroEnv):
    assert env.resets_to_load_state
    first = env.reset()[0]
    for _ in range(5):
        env.step(act(RIGHT, shot=1))
    again, info = env.reset()
    assert np.array_equal(first, again)
    assert info["ram"] == {"score": 0} and ram(env)[:4] == bytes(4), "the score is back to 0"


def test_savestate_round_trip_reproduces_frames_and_ram(env: RetroEnv):
    env.reset()
    for action in (act(shot=1), act(RIGHT), act(DOWN_RIGHT, shot=1)):
        env.step(action)
    state = env.save_state()
    actions = [act(shot=1), act(DOWN_RIGHT), act(), act(bomb=1), act(shot=1, bomb=1)]

    def play() -> list[tuple[bytes, bytes, float]]:
        steps = [env.step(a) for a in actions]
        return [(s[0].tobytes(), ram(env), s[1]) for s in steps]

    obs, info = env.reset(options={"state": state})
    first = play()
    assert frame_number(obs) == 6, "the state was at frame 5"
    assert info == {"episode_frame": 1, "ram": {"score": 2}}
    assert [s[2] for s in first] == [1.0, 0.0, 0.0, 0.0, 1.0], "rewards from the state's score"
    assert np.array_equal(env.reset(options={"state": state})[0], obs)
    assert play() == first
    with pytest.raises(HarnessError, match="savestate"):
        env.reset(options={"state": state[:10]})


def test_deterministic_over_1000_frames(env: RetroEnv):
    env.reset()
    env.step(act(RIGHT))
    state = env.save_state()

    def run() -> str:
        digest = hashlib.sha256()
        digest.update(env.reset(options={"state": state})[0].tobytes())
        for i in range(1000):
            h = (i * 2_654_435_761) >> 16
            obs = env.step(act(h % 9, (h >> 4) & 1, (h >> 5) & 1))[0]
            digest.update(obs.tobytes())
            digest.update(ram(env))
        return digest.hexdigest()

    assert run() == run()


def test_second_core_in_a_process_fails_cleanly(env: RetroEnv, testcore_path: Path, tmp_path):
    with pytest.raises(CoreLoadError, match="another core is loaded"):
        make(testcore_path, tmp_path)
    env.reset()
    env.step(act(shot=1))  # the first is untouched
    env.close()
    env.close()  # idempotent
    with pytest.raises(ValueError, match="closed"):
        env.step(act(shot=1))
    with retro_env(testcore_path, tmp_path) as second:
        assert second.reset()[1]["episode_frame"] == 2


def test_resize_rotate_and_crop(testcore_path: Path, tmp_path: Path):
    with retro_env(testcore_path, tmp_path) as full:
        full.reset()
        frame = full.step(act(shot=1))[0]
    # The A cell is 20 pixels wide at x = 160..180, y = 0..20.
    cropped = config(crop="crop = [160, 0, 20, 20]")
    with retro_env(testcore_path, tmp_path, cropped, obs_size=(20, 20)) as crop:
        crop.reset()
        assert np.array_equal(crop.step(act(shot=1))[0], frame[:20, 160:180])
    with retro_env(testcore_path, tmp_path, config(90), obs_size=(WIDTH, HEIGHT)) as tate:
        tate.reset()
        assert np.array_equal(tate.step(act(shot=1))[0], np.rot90(frame, k=-1))
    with retro_env(testcore_path, tmp_path, obs_size=(84, 84), interpolation="bilinear") as small:
        assert small.reset()[0].shape == (84, 84, 3)
        assert small.observation_space.shape == (84, 84, 3)
    for bad in ({"interpolation": "cubic"}, {"obs_size": (0, 4)}):
        with pytest.raises((ValueError, HarnessError)), retro_env(testcore_path, tmp_path, **bad):
            pass
    off_screen = config(crop="crop = [300, 0, 40, 10]")
    with (
        retro_env(testcore_path, tmp_path, off_screen) as bad_crop,
        pytest.raises(HarnessError, match="crop"),
    ):
        bad_crop.reset()


@pytest.mark.parametrize("action_format", ["dict", "flat"])
def test_gymnasium_check_env(testcore_path: Path, tmp_path: Path, action_format: str):
    options = {"obs_size": (64, 64), "frameskip": 2, "action_format": action_format}
    with retro_env(testcore_path, tmp_path, **options) as env:
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
    with retro_env(
        testcore_path, tmp_path, obs_size=(256, 256), frameskip=4, action_format="flat"
    ) as env:
        actions = [i % env.action_space.n for i in range(500)]
        env.reset()
        start = time.perf_counter()
        for action in actions:
            env.step(action)
        elapsed = time.perf_counter() - start
    profile = _native.build_info()["profile"]
    print(
        f"\nRetroEnv on gth-testcore: {len(actions) / elapsed:.0f} steps/s"
        f" ({profile} build, frameskip 4, 256x256, RAM read and reward every step)"
    )
