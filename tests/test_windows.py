"""Sequence windows over training shards (docs/training.md): a testcore recording with one
unlabelled step, so its shard has two segments, read back as windows."""

import functools
import json
import struct
from pathlib import Path

import numpy as np
import pytest

from gtharness import _native, rewards
from gtharness.env import RetroEnv
from gtharness.errors import ConfigError
from gtharness.record import Record
from gtharness.rewards import score_delta
from gtharness.shards import Shard, ShardOptions, export_shards
from gtharness.train import Windows, returns_to_go, sample_rewards

FRAMESKIP = 3
STEPS = 60
START_STEP = 25
"""The step whose frames hold start, which no slot maps: sample START_STEP - 1, whose next
frame is that step's first, has no label, so the shard's samples split into two segments."""
START = 1 << 3  # RETRO_DEVICE_ID_JOYPAD_START
CHUNK = 8


@pytest.fixture
def shard(testcore_path: Path, testcore_data: Path, tmp_path: Path, zstd_raw_frame) -> Path:
    """A shard of one episode: STEPS random flat actions, start added to step START_STEP."""
    env = RetroEnv.from_game(
        "testcore",
        testcore_data,
        frameskip=FRAMESKIP,
        obs_size=(60, 80),
        action_format="flat",
        reward=score_delta,
        sticky_prob=0.0,
        max_episode_frames=None,
        noop_max=0,
        maxpool=False,
        grayscale=False,
        core_path=testcore_path,
        record=Record(
            directory=tmp_path / "rec",
            recorder="scripted",
            player="random",
            license="CC0-1.0",
            checks_every=60,
        ),
    )
    rng = np.random.default_rng(4)
    try:
        table = env.config.joypad_table()
        env.reset()
        masks = [0]  # the reset's step
        for _ in range(STEPS):
            action = int(rng.integers(0, env.config.n_actions))
            env.step(action)
            masks.append(table[action])
    finally:
        env.close()
    (recording,) = env.recordings
    # Rewrite inputs.bin (docs/recording-format.md, "inputs.bin") with start held through
    # step START_STEP; start changes no test-core state (docs/libretro.md, "Test core"), so
    # the replay still matches its checks.
    inputs = bytearray(b"GTHINPUT")
    for step, mask in enumerate(masks):
        held = mask | (START if step == START_STEP else 0)
        for i in range(FRAMESKIP):
            inputs += struct.pack("<HB", held, int(i == FRAMESKIP - 1))
    (recording / "inputs.bin").write_bytes(zstd_raw_frame(bytes(inputs)))
    options = ShardOptions(
        obs_size=(60, 80),
        crop="game",
        orientation="game",
        frameskip="recorded",
        filter="all",
        chunk_samples=CHUNK,
        interpolation="area",
        maxpool=False,
        grayscale=False,
        sprites=False,
    )
    (result,) = export_shards(
        [recording], tmp_path / "shards", testcore_data, options, jobs=1, core_path=testcore_path
    )
    assert result.error is None and result.summary is not None
    assert result.summary["unlabelled"] == 1
    return result.shard


def all_frames(shard: Shard) -> np.ndarray:
    return np.concatenate([shard.frames(c) for c in range(shard.frame_chunks)])


def test_the_fixture_shard_has_two_segments(shard: Path):
    with Shard(shard) as s:
        assert len(s) == STEPS - 1
        first = START_STEP - 1
        np.testing.assert_array_equal(s.segment, (np.arange(STEPS - 1) >= first).astype(np.uint32))


@pytest.mark.parametrize(("length", "stride"), [(1, 1), (5, 1), (5, 3), (10, 10), (24, 2)])
def test_windows_stay_in_one_segment_and_hold_the_shard_s_samples(
    shard: Path, length: int, stride: int
):
    with (
        Shard(shard) as s,
        Windows(
            [shard],
            length=length,
            stride=stride,
            reward=score_delta,
            action_format="flat",
            cache_chunks=4,
        ) as windows,
    ):
        segment, frames = s.segment, all_frames(s)
        # Segment 0 is samples 0..first-1, segment 1 the rest.
        first = START_STEP - 1
        expected = [
            start
            for lo, hi in ((0, first), (first, len(s)))
            for start in range(lo, hi - length + 1, stride)
        ]
        assert [windows.location(i) for i in range(len(windows))] == [(0, x) for x in expected]
        for i, start in enumerate(expected):
            window = windows.window(i)
            run = slice(start, start + length)
            assert len(set(segment[run].tolist())) == 1
            assert (int(window["shard"]), int(window["sample"])) == (0, start)
            np.testing.assert_array_equal(window["frame"], frames[run])
            np.testing.assert_array_equal(
                window["action"], s.direction[run].astype(np.int64) * 16 + s.buttons[run]
            )
            np.testing.assert_array_equal(window["timestep"], np.arange(start, start + length))
            np.testing.assert_array_equal(window["episode_frame"], s.episode_frame[run])
            np.testing.assert_array_equal(window["confidence"], s.confidence[run])
            assert window["frame"].shape == (length, 60, 80, 3)
            assert "ram" not in window


def test_a_segment_shorter_than_the_window_gives_none(shard: Path):
    # Segment 0 has START_STEP - 1 = 24 samples, segment 1 has 35.
    with Windows(
        [shard], length=30, stride=1, reward=score_delta, action_format="flat", cache_chunks=5
    ) as windows:
        assert [windows.location(i)[1] for i in range(len(windows))] == list(range(24, 30))


def test_rewards_and_returns_to_go_sum_over_the_episode(shard: Path):
    with Shard(shard) as s:
        score = s.ram[:, s.meta["ram_names"].index("score")]
        assert score[-1] > score[0]  # the random actions held A sometimes
        r = sample_rewards(s, score_delta)
        # Each sample's reward is the score change to the next sample, across the gap too;
        # the last sample has no RAM after it.
        np.testing.assert_array_equal(r[:-1], np.diff(score).astype(np.float64))
        assert r[-1] == 0
        rtg = returns_to_go(r)
        np.testing.assert_array_equal(rtg, [r[i:].sum() for i in range(len(r))])
        # score_delta telescopes: the return from sample i is the score still to come.
        np.testing.assert_array_equal(rtg, (score[-1] - score).astype(np.float64))
    with Windows(
        [shard], length=6, stride=4, reward=score_delta, action_format="flat", cache_chunks=2
    ) as windows:
        for i in range(len(windows)):
            window, (_, start) = windows.window(i), windows.location(i)
            np.testing.assert_array_equal(window["reward"], r[start : start + 6])
            np.testing.assert_array_equal(window["return_to_go"], rtg[start : start + 6])


def test_dict_actions_are_the_env_s(shard: Path):
    with (
        Shard(shard) as s,
        Windows(
            [shard], length=4, stride=4, reward=score_delta, action_format="dict", cache_chunks=2
        ) as windows,
    ):
        assert len(windows) and "action" not in windows.window(0)
        for i in range(len(windows)):
            window, (_, start) = windows.window(i), windows.location(i)
            np.testing.assert_array_equal(window["direction"], s.direction[start : start + 4])
            assert window["buttons"].shape == (4, 4)
            assert set(window["buttons"].ravel().tolist()) <= {0, 1}
            bits = window["buttons"].astype(np.uint32) @ (1 << np.arange(4, dtype=np.uint32))
            np.testing.assert_array_equal(bits, s.buttons[start : start + 4])


def test_batches_hold_every_window_once(shard: Path):
    with Windows(
        [shard, shard], length=5, stride=2, reward=score_delta, action_format="flat", cache_chunks=2
    ) as windows:
        count = len(windows)
        with Windows(
            [shard], length=5, stride=2, reward=score_delta, action_format="flat", cache_chunks=2
        ) as one:
            assert count == 2 * len(one)
        seen = []
        for batch in windows.batches(7, rng=np.random.default_rng(3), buffer_chunks=2):
            assert len(batch["frame"]) <= 7 and batch["frame"].shape[1:] == (5, 60, 80, 3)
            for row in range(len(batch["frame"])):
                location = (int(batch["shard"][row]), int(batch["sample"][row]))
                index = [windows.location(i) for i in range(count)].index(location)
                expected = windows.window(index)
                for key, value in expected.items():
                    np.testing.assert_array_equal(batch[key][row], value)
                seen.append(location)
        locations = [windows.location(i) for i in range(count)]
        assert sorted(seen) == locations and seen != locations
        assert {s for s, _ in seen} == {0, 1}
        ordered = list(windows.batches(16, rng=None, buffer_chunks=1))
        assert [len(b["frame"]) for b in ordered] == [16] * (count // 16) + [count % 16]
        assert [
            (int(s), int(x)) for b in ordered for s, x in zip(b["shard"], b["sample"], strict=True)
        ] == locations


def test_arguments_are_checked(shard: Path, monkeypatch: pytest.MonkeyPatch):
    def make(**changes):
        fields = {
            "length": 10,
            "stride": 1,
            "reward": score_delta,
            "action_format": "flat",
            "cache_chunks": 3,
        } | changes
        return Windows([shard], **fields)

    with pytest.raises(ValueError, match="cache_chunks must be at least 3"):
        make(length=10, cache_chunks=2)  # samples 7-16 span chunks 0-2
    with pytest.raises(ValueError, match="length and stride"):
        make(stride=0)
    with pytest.raises(ValueError, match="action_format"):
        make(action_format="multi")

    def lives_lost(previous, current) -> float:
        return float(previous["lives"] - current["lives"])

    monkeypatch.setitem(rewards.REQUIRED_RAM, lives_lost, ("lives",))
    with pytest.raises(ConfigError, match=r"needs the RAM variables \['lives'\]"):
        make(reward=functools.partial(lives_lost))


def test_a_window_is_read_only(shard: Path):
    # Windows overlap and share arrays (and the chunk cache), so a write must raise, not
    # change the next window over the same samples.
    with Windows(
        [shard], length=6, stride=2, reward=score_delta, action_format="dict", cache_chunks=2
    ) as windows:
        for index in (0, 2):  # samples 0-5 in chunk 0, then 4-9 across chunks 0 and 1
            window = windows.window(index)
            before = {key: np.array(value) for key, value in window.items()}
            assert windows.location(2) == (0, 4)
            for value in window.values():
                if np.ndim(value) == 0:
                    continue  # shard and sample are numpy scalars, immutable
                with pytest.raises(ValueError, match="read-only"):
                    value[...] = 0
            again = windows.window(index)
            for key, value in before.items():
                np.testing.assert_array_equal(again[key], value)
        batch = next(windows.batches(4, rng=None, buffer_chunks=1))
        batch["return_to_go"] *= 2  # a batch is a copy
        np.testing.assert_array_equal(
            batch["return_to_go"][0], 2 * windows.window(0)["return_to_go"]
        )


def test_a_dataset_reward_reads_the_sample_s_own_column(tmp_path: Path):
    n = 12
    reward = np.array([0, 1, 0, 0, -1, 0, 2, 0, 0, 0, 0, 1], dtype=np.int64)
    meta = {
        "source_id": "synthetic",
        "episode_id": 0,
        "game_id": "atari-pong",
        "fps": 60.0,
        "frameskip": 4,
        "obs_size": [4, 4],
        "crop": None,
        "orientation": 0,
        "interpolation": "area",
        "maxpool": True,
        "grayscale": True,
        "ram_names": ["reward", "terminal"],
        "terminated": True,
        "dataset": {},
    }
    path = tmp_path / "d.npz"
    _native.write_dataset_shard(
        path,
        json.dumps(meta),
        frames=np.zeros((n, 4, 4, 1), dtype=np.uint8),
        direction=np.zeros(n, dtype=np.uint8),
        buttons=np.zeros(n, dtype=np.uint8),
        ram=np.stack([reward, np.arange(n) == n - 1], axis=1).astype(np.int64),
        episode_frame=(4 * np.arange(n)).astype(np.uint64),
        segment=np.zeros(n, dtype=np.uint32),
        chunk_samples=5,
    )
    with Shard(path) as s:
        # The terminal step's reward is the last sample's own: it isn't dropped.
        for r in (rewards.dataset_reward, functools.partial(rewards.dataset_reward)):
            np.testing.assert_array_equal(sample_rewards(s, r), reward)
        with pytest.raises(ConfigError, match=r"\['score'\]"):
            sample_rewards(s, score_delta)
    with Windows(
        [path],
        length=4,
        stride=4,
        reward=rewards.dataset_reward,
        action_format="flat",
        cache_chunks=2,
    ) as windows:
        assert len(windows) == 3
        last = windows.window(2)
        np.testing.assert_array_equal(last["reward"], reward[8:])
        np.testing.assert_array_equal(last["return_to_go"], np.cumsum(reward[::-1])[::-1][8:])
        assert windows.window(0)["return_to_go"][0] == reward.sum() == 3
