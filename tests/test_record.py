"""Recording episodes with RetroEnv(record=...) and replaying them (docs/recording-format.md),
on games/testcore/game.toml, whose content seeds the test core's PRNG so its RAM changes
every frame."""

import contextlib
import hashlib
import struct
import tomllib
from collections.abc import Callable, Iterator
from pathlib import Path

import numpy as np
import pytest

from gtharness import _native
from gtharness.env import GAMES_DIR, RetroEnv
from gtharness.errors import ConfigError, DesyncError, HarnessError
from gtharness.record import Record, Replayer
from gtharness.rewards import score_delta

FRAMESKIP = 3
A = 1 << 8


def record(directory: Path, checks_every: int = 60) -> Record:
    return Record(
        directory=directory,
        recorder="scripted",
        player="random",
        license="CC0-1.0",
        checks_every=checks_every,
    )


@pytest.fixture
def content(testcore_data: Path) -> Path:
    """The content file testcore_data put in the ROM directory."""
    return testcore_data.parent / "roms" / "sub" / "renamed.gthc"


def play(
    testcore_path: Path, testcore_data: Path, rec: Record, episodes: int, steps: int
) -> tuple[list[Path], list[list[np.ndarray]], list[list[int]]]:
    """Plays random flat actions; returns the recordings, every episode's observations
    (reset's first) and the joypad mask of every step."""
    env = RetroEnv.from_game(
        "testcore",
        testcore_data,
        frameskip=FRAMESKIP,
        obs_size=(60, 80),
        action_format="flat",
        reward=score_delta,
        core_path=testcore_path,
        record=rec,
    )
    rng = np.random.default_rng(5)
    observations: list[list[np.ndarray]] = []
    masks: list[list[int]] = []
    try:
        table = env.config.joypad_table()
        for _ in range(episodes):
            obs, _ = env.reset()
            observations.append([obs])
            masks.append([0])
            for _ in range(steps):
                action = int(rng.integers(env.config.n_actions))
                obs, *_ = env.step(action)
                observations[-1].append(obs)
                masks[-1].append(table[action])
    finally:
        env.close()
    return env.recordings, observations, masks


@contextlib.contextmanager
def replayer(path: Path, testcore_path: Path, content: Path, **kwargs) -> Iterator[Replayer]:
    """A replayer of ``path`` at the game's crop and orientation, closed afterwards (one core
    per process)."""
    game = _native.GameConfig.load(GAMES_DIR / "testcore" / "game.toml")
    options = {
        "obs_height": 60,
        "obs_width": 80,
        "crop": game.crop,
        "rotate": game.orientation,
        "interpolation": "area",
    } | kwargs
    replay = Replayer(path, testcore_path, content, content.parent, **options)
    try:
        yield replay
    finally:
        replay.close()


def replay_all(replay: Replayer) -> Iterator[tuple[int, int, np.ndarray]]:
    while (step := replay.step()) is not None:
        yield step


def test_episodes_are_recorded_and_replay_the_same_frames(
    testcore_path: Path, testcore_data: Path, content: Path, tmp_path: Path
):
    out = tmp_path / "recordings"
    paths, observations, masks = play(testcore_path, testcore_data, record(out), 2, 200)
    assert len(paths) == 2 and all(path.parent == out for path in paths)
    assert all(path.name.startswith("testcore-") for path in paths)
    numbers = [int(path.name.removesuffix(".gthrec").rsplit("-", 1)[1]) for path in paths]
    assert numbers[1] == numbers[0] + 1
    for path, episode, episode_masks in zip(paths, observations, masks, strict=True):
        with replayer(path, testcore_path, content) as replay:
            assert (replay.frames, replay.steps) == (201 * FRAMESKIP, 201)
            meta = tomllib.loads(replay.meta_toml)
            game_toml = replay.game.to_toml()
            replayed = list(replay_all(replay))
            assert replay.frame == replay.frames
        assert replay.closed
        assert (meta["game_id"], meta["recorder"], meta["player"]) == (
            "testcore",
            "scripted",
            "random",
        )
        assert meta["license"] == "CC0-1.0" and meta["harness"] == _native.build_info()
        assert meta["content"]["sha1"] == hashlib.sha1(content.read_bytes()).hexdigest()
        assert game_toml == (path / "game.toml").read_text(encoding="utf-8")
        assert [mask for mask, _, _ in replayed] == episode_masks
        assert [frames for _, frames, _ in replayed] == [FRAMESKIP] * 201
        assert all(np.array_equal(a, b) for (*_, a), b in zip(replayed, episode, strict=True))

    # Any size and rotation, from the same recording.
    with replayer(paths[0], testcore_path, content, obs_height=100, obs_width=50, rotate=0) as r:
        frames = [obs for *_, obs in replay_all(r)]
    assert len(frames) == 201 and frames[0].shape == (100, 50, 3)


def test_a_tampered_input_raises_desync_error_at_its_frame(
    testcore_path: Path,
    testcore_data: Path,
    content: Path,
    tmp_path: Path,
    zstd_raw_frame: Callable[[bytes], bytes],
):
    paths, _, masks = play(testcore_path, testcore_data, record(tmp_path, checks_every=1), 1, 100)
    # Rewrite inputs.bin (as an uncompressed zstd frame) with A toggled on frame 151.
    inputs = bytearray(b"GTHINPUT")
    for mask in masks[0]:
        for i in range(FRAMESKIP):
            inputs += struct.pack("<HB", mask, int(i == FRAMESKIP - 1))
    tampered = 151
    at = 8 + 3 * (tampered - 1)
    inputs[at : at + 2] = struct.pack("<H", struct.unpack_from("<H", inputs, at)[0] ^ A)
    (paths[0] / "inputs.bin").write_bytes(zstd_raw_frame(bytes(inputs)))

    with replayer(paths[0], testcore_path, content) as replay:
        with pytest.raises(DesyncError, match=f"frame {tampered}: RAM variable score"):
            replay.run_to_end()
        assert replay.frame == tampered


def test_replay_refuses_other_content(
    testcore_path: Path, testcore_data: Path, content: Path, tmp_path: Path
):
    paths, _, _ = play(testcore_path, testcore_data, record(tmp_path), 1, 10)
    other = tmp_path / "other.gthc"
    other.write_bytes(b"other content")
    with pytest.raises(ConfigError, match="SHA-1"), replayer(paths[0], testcore_path, other):
        pass
    with pytest.raises(ConfigError, match="none was given"):
        Replayer(paths[0], testcore_path, None, tmp_path, 60, 80, None, 0, "area")
    with replayer(paths[0], testcore_path, content) as replay:
        assert replay.run_to_end() == 11 * FRAMESKIP


def test_a_failed_recording_raises_and_the_next_episode_records(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    env = RetroEnv.from_game(
        "testcore",
        testcore_data,
        frameskip=FRAMESKIP,
        obs_size=(60, 80),
        action_format="flat",
        reward=score_delta,
        core_path=testcore_path,
        record=record(tmp_path),
    )
    try:
        env.reset()
        env.step(0)
        # A directory where meta.toml goes makes finishing fail on every OS.
        (failed,) = tmp_path.glob("*.gthrec")
        (failed / "meta.toml").mkdir()
        with pytest.raises(HarnessError, match=r"meta\.toml"):
            env.reset()
        env.reset()
        env.step(0)
    finally:
        env.close()
    assert len(env.recordings) == 1 and env.recordings[0] != failed
    assert (env.recordings[0] / "meta.toml").is_file()


def test_recording_needs_its_choices(testcore_path: Path, testcore_data: Path, tmp_path: Path):
    game = _native.GameConfig.load(GAMES_DIR / "testcore" / "game.toml")
    options = {
        "core_path": testcore_path,
        "content_path": None,
        "system_dir": tmp_path,
        "frameskip": 1,
        "obs_size": (60, 80),
        "action_format": "flat",
        "reward": score_delta,
        "reset_state": None,
    }
    with pytest.raises(ValueError, match="game_id"):
        RetroEnv(game, record=record(tmp_path), **options)

    core = _native.Core(testcore_path, None, tmp_path, {}, 60, 80, None, 0, "area")
    try:
        with pytest.raises(ValueError, match="recorder must be one of"):
            core.start_recording(tmp_path / "a.gthrec", game, "testcore", None, "bot", "p", "l", 60)
        with pytest.raises(ValueError, match="checks_every"):
            core.start_recording(
                tmp_path / "a.gthrec", game, "testcore", None, "policy", "p", "l", 0
            )
        assert core.finish_recording() is None
        core.start_recording(tmp_path / "a.gthrec", game, "testcore", None, "policy", "p", "l", 60)
        assert core.recording == tmp_path / "a.gthrec"
        with pytest.raises(ValueError, match="finish the recording first"):
            core.reset()
        with pytest.raises(ValueError, match="finish the recording first"):
            core.unserialize(core.serialize())
        core.step(5, 0)
        assert core.finish_recording() == tmp_path / "a.gthrec"
        assert core.recording is None
        meta = tomllib.loads((tmp_path / "a.gthrec" / "meta.toml").read_text(encoding="utf-8"))
        assert (meta["frames"], meta["steps"], meta["recorder"]) == (5, 1, "policy")
        assert "content" not in meta
    finally:
        core.close()
