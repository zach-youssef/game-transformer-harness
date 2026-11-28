"""gtharness.importers.dqn_replay: DQN-Replay checkpoints, written here the way Dopamine saves
its replay buffer, imported as shards and read back through the training windows."""

from __future__ import annotations

import gzip
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import numpy as np
import pytest

from gtharness import _native, cli
from gtharness.env import GAMES_DIR
from gtharness.errors import ConfigError
from gtharness.importers import dqn_replay
from gtharness.rewards import dataset_reward
from gtharness.shards import Shard
from gtharness.train import Windows

PAD = dqn_replay.PADDING


def _save(path: Path, array: np.ndarray) -> None:
    with gzip.open(path, "wb") as f:
        np.save(f, array, allow_pickle=False)


class Buffer:
    """Dopamine's circular buffer: each episode's transitions after PADDING zero ones."""

    def __init__(self, capacity: int, seed: int) -> None:
        self.capacity = capacity
        self.rng = np.random.default_rng(seed)
        self.observation = np.zeros((capacity, 84, 84), dtype=np.uint8)
        self.action = np.zeros(capacity, dtype=np.int32)
        self.reward = np.zeros(capacity, dtype=np.float32)
        self.terminal = np.zeros(capacity, dtype=np.uint8)
        self.add_count = 0
        self.log: list[tuple[np.ndarray, int, float, int] | None] = []
        """Every transition added, oldest first; None for padding."""

    def _add(self, obs: np.ndarray, action: int, reward: float, terminal: int) -> None:
        i = self.add_count % self.capacity
        self.observation[i], self.action[i], self.reward[i], self.terminal[i] = (
            obs,
            action,
            reward,
            terminal,
        )
        self.add_count += 1

    def episode(self, steps: int, terminal: bool = True) -> None:
        for _ in range(PAD):
            self._add(np.zeros((84, 84), np.uint8), 0, 0.0, 0)
            self.log.append(None)
        for t in range(steps):
            obs = self.rng.integers(1, 256, (84, 84), dtype=np.uint8)
            action = int(self.rng.integers(0, 6))
            reward = float(self.rng.choice([-1, 0, 0, 1, 1]))
            end = int(terminal and t == steps - 1)
            self._add(obs, action, reward, end)
            self.log.append((obs, action, reward, end))

    def save(self, root: Path, checkpoint: int) -> Path:
        logs = root / "Pong" / "1" / "replay_logs"
        logs.mkdir(parents=True)
        # Dopamine's names (its STORE_FILENAME_PREFIX is "$store$_"), spelled out here.
        for name, array in [
            ("$store$_observation", self.observation),
            ("$store$_action", self.action),
            ("$store$_reward", self.reward),
            ("$store$_terminal", self.terminal),
        ]:
            _save(logs / f"{name}_ckpt.{checkpoint}.gz", array)
        _save(logs / f"add_count_ckpt.{checkpoint}.gz", np.array(self.add_count))
        _save(logs / f"invalid_range_ckpt.{checkpoint}.gz", np.zeros(4))
        return logs

    def held(self) -> list[tuple[np.ndarray, int, float, int] | None]:
        """The buffer's transitions, oldest first."""
        return self.log[-min(self.add_count, self.capacity) :]


def _pong():
    config = _native.GameConfig.load(GAMES_DIR / "atari-pong" / "game.toml")
    return [config.from_atari(a) for a in config.atari_minimal_actions]


def test_a_wrapped_buffer_s_slice_reads_back_through_the_windows(tmp_path: Path):
    buffer = Buffer(capacity=60, seed=1)
    for steps in (9, 14, 6, 20, 11):
        buffer.episode(steps)
    buffer.episode(7, terminal=False)  # cut by the checkpoint
    assert buffer.add_count > buffer.capacity  # wrapped: the oldest entries are overwritten
    logs = buffer.save(tmp_path / "data", 50)
    assert dqn_replay.source(logs, 50).path("observation") == (
        logs.resolve() / "$store$_observation_ckpt.50.gz"
    )
    held = buffer.held()
    # From inside an overwritten episode, across padding, to inside the last episode.
    start, count = 1, 54
    result = dqn_replay.import_slice(
        dqn_replay.source(logs, 50),
        tmp_path / "shards",
        game_id="atari-pong",
        start=start,
        count=count,
        chunk_samples=8,
    )
    part = held[start : start + count]
    transitions = [t for t in part if t is not None]
    assert result.samples == len(transitions)
    assert result.padding == count - len(transitions) > 0
    assert result.terminals == sum(t[3] for t in transitions) == 4
    assert part[0] is not None and part[-1] is not None and not part[-1][3]
    assert result.reward_sum == sum(t[2] for t in transitions) != 0

    # One shard per episode piece, cut after each terminal; names from the first transition.
    assert len(result.shards) == result.terminals + 1
    assert result.shards[0].name == f"dqn-replay-Pong-1-ckpt50_{start:07}.npz"
    flat = _pong()
    frames, actions, rewards, ends = [], [], [], []
    for path in result.shards:
        with Shard(path) as shard:
            meta = shard.meta
            assert (meta["label_source"], meta["shard_version"]) == ("dataset", 4)
            assert meta["ram_names"] == ["reward", "terminal"]
            assert (meta["obs_size"], meta["channels"], meta["frameskip"]) == ([84, 84], 1, 4)
            assert meta["dataset"]["checkpoint"] == 50
            assert meta["dataset"]["add_count"] == buffer.add_count
            terminal = shard.ram[:, 1]
            assert meta["terminated"] == bool(terminal[-1]) and not terminal[:-1].any()
            ends.append(meta["terminated"])
            # Each piece's episode frames step by 4 from where its episode is.
            assert np.all(np.diff(shard.label("episode_frame").astype(np.int64)) == 4)
    with Windows(
        result.shards,
        length=1,
        stride=1,
        reward=dataset_reward,
        action_format="flat",
        cache_chunks=2,
    ) as windows:
        assert len(windows) == len(transitions)
        for i in range(len(windows)):
            window = windows.window(i)
            frames.append(window["frame"][0, ..., 0])
            actions.append(int(window["action"][0]))
            rewards.append(float(window["reward"][0]))
    np.testing.assert_array_equal(frames, [t[0] for t in transitions])
    assert actions == [flat[t[1]] for t in transitions]
    assert rewards == [t[2] for t in transitions]
    assert sum(ends) == result.terminals and ends[-1] is False
    assert len(ends) == 5

    # The first episode began before the buffer's oldest transition: its episode frames
    # count from there. The others start inside the slice, at 0.
    with Shard(result.shards[0]) as shard:
        assert not shard.meta["dataset"]["episode_start_known"]
        assert shard.label("episode_frame")[0] == 4 * start
    for path in result.shards[1:]:
        with Shard(path) as shard:
            assert shard.label("episode_frame")[0] == 0
            assert shard.meta["dataset"]["episode_start_known"]

    report = dqn_replay.verify(result)
    assert report["ok"], report
    assert report["reward_sum"] == report["source_reward_sum"]
    assert report["terminated_shards"] == report["source_terminals"]
    # A difference from the source fails the check.
    result.expected["reward"][0] += 1
    report = dqn_replay.verify(result)
    assert not report["ok"] and report["checks"] == dict.fromkeys(report["checks"], True) | {
        "reward": False
    }


def test_a_wrapped_buffer_can_start_inside_padding(tmp_path: Path):
    buffer = Buffer(capacity=30, seed=5)
    for steps in (9, 8, 5):
        buffer.episode(steps)
    assert buffer.held()[: PAD - 1] == [None] * (PAD - 1) and buffer.held()[PAD - 1]
    logs = buffer.save(tmp_path / "data", 0)
    result = dqn_replay.import_slice(
        dqn_replay.source(logs, 0),
        tmp_path / "shards",
        game_id="atari-pong",
        start=0,
        count=30,
        chunk_samples=4,
    )
    assert (result.samples, result.padding, len(result.shards)) == (22, PAD - 1 + 2 * PAD, 3)
    with Shard(result.shards[0]) as shard:
        assert shard.label("episode_frame")[0] == 0
        assert not shard.meta["dataset"]["episode_start_known"]
    assert dqn_replay.verify(result)["ok"]
    # A slice after the head reads the head's frames and its own in one pass, apart.
    later = dqn_replay.import_slice(
        dqn_replay.source(logs, 0),
        tmp_path / "later",
        game_id="atari-pong",
        start=8,
        count=12,
        chunk_samples=4,
    )
    transitions = [t for t in buffer.held()[8:20] if t is not None]
    np.testing.assert_array_equal(later.expected["frame"][..., 0], [t[0] for t in transitions])
    with Shard(later.shards[0]) as shard:
        assert shard.label("episode_frame")[0] == 4 * (8 - (PAD - 1))
    assert dqn_replay.verify(later)["ok"]


def test_an_unwrapped_buffer_starts_with_an_episode(tmp_path: Path):
    buffer = Buffer(capacity=100, seed=2)
    buffer.episode(10)
    buffer.episode(5)
    logs = buffer.save(tmp_path / "data", 3)
    result = dqn_replay.import_slice(
        dqn_replay.source(logs, 3),
        tmp_path / "shards",
        game_id="atari-pong",
        start=0,
        count=buffer.add_count,
        chunk_samples=4,
    )
    assert (result.samples, result.padding, result.terminals) == (15, 2 * PAD, 2)
    with Shard(result.shards[0]) as first, Shard(result.shards[1]) as second:
        assert (first.meta["episode_id"], second.meta["episode_id"]) == (0, 1)
        assert first.label("episode_frame")[0] == 0 and second.label("episode_frame")[0] == 0
        assert first.meta["dataset"]["episode_start_known"]
        assert first.meta["terminated"] and second.meta["terminated"]
    assert dqn_replay.verify(result)["ok"]


def test_the_command_writes_and_verifies(tmp_path: Path, capsys: pytest.CaptureFixture[str]):
    buffer = Buffer(capacity=40, seed=3)
    buffer.episode(8)
    buffer.episode(12, terminal=False)
    logs = buffer.save(tmp_path / "data", 7)
    argv = [
        "import-dqn-replay", str(logs), "--checkpoint", "7", "--game", "atari-pong",
        "--start", "2", "--count", "20", "--out", str(tmp_path / "shards"),
        "--chunk-samples", "16", "--verify",
    ]  # fmt: skip
    assert cli.main(argv) == 0
    report = json.loads(capsys.readouterr().out)
    assert report["verify"]["ok"] and report["samples"] == report["verify"]["windows"]
    # Existing shards aren't replaced.
    assert cli.main(argv) == 2
    assert "already exist" in capsys.readouterr().err


def test_bad_sources_are_refused(tmp_path: Path):
    buffer = Buffer(capacity=40, seed=4)
    buffer.episode(10)
    logs = buffer.save(tmp_path / "data", 1)
    src = dqn_replay.source(logs, 1)

    def run(**changes):
        kwargs = dict(game_id="atari-pong", start=0, count=13, chunk_samples=4) | changes
        return dqn_replay.import_slice(src, tmp_path / "out", **kwargs)

    with pytest.raises(ConfigError, match="past the buffer"):
        run(count=14)
    with pytest.raises(ConfigError, match="all padding"):
        run(count=PAD)
    with pytest.raises(ConfigError, match="minimal_actions"):
        run(game_id="testcore")
    with pytest.raises(ConfigError, match="missing DQN-Replay files"):
        dqn_replay.source(logs, 2)
    with pytest.raises(ConfigError, match="replay_logs directory"):
        dqn_replay.source(tmp_path, 1)

    def resave(name: str, array: np.ndarray) -> None:
        _save(logs / f"$store$_{name}_ckpt.1.gz", array)

    reward = buffer.reward.copy()
    reward[PAD] = 0.5
    resave("reward", reward)
    with pytest.raises(ConfigError, match="whole number"):
        run()
    resave("reward", buffer.reward)
    action = buffer.action.copy()
    action[PAD + 1] = 6
    resave("action", action)
    with pytest.raises(ConfigError, match="minimal set of 6"):
        run()
    resave("action", buffer.action)
    obs = buffer.observation.copy()
    obs[1, 0, 0] = 1
    resave("observation", obs)
    with pytest.raises(ConfigError, match="transition 1 should be Dopamine's zero padding"):
        run()
    resave("observation", buffer.observation[..., None])
    with pytest.raises(ConfigError, match="observation"):
        run()
    assert not (tmp_path / "out").exists()


@pytest.mark.skipif(sys.platform == "win32" or shutil.which("bash") is None, reason="a bash script")
@pytest.mark.parametrize("budget, status", [(5000, 0), (4999, 1)])
def test_the_fetch_script_copies_only_within_the_budget(tmp_path: Path, budget: int, status: int):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    log = tmp_path / "gsutil.log"
    # A gsutil that sizes every file at 1000 bytes and logs the copy.
    gsutil = bin_dir / "gsutil"
    gsutil.write_text(
        "#!/usr/bin/env bash\n"
        'if [ "$1" = du ]; then shift 2; for f in "$@"; do echo "1000  $f"; done;'
        ' echo "$(( $# * 1000 ))  total"; else echo "$@" >> "$GSUTIL_LOG"; fi\n'
    )
    gsutil.chmod(0o755)
    script = Path(__file__).parent.parent / "scripts" / "fetch-dqn-replay.sh"
    env = os.environ | {
        "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
        "GSUTIL_LOG": str(log),
    }
    run = subprocess.run(
        ["bash", str(script), "Pong", "1", "50", str(tmp_path / "data"), str(budget)],
        env=env,
        capture_output=True,
        text=True,
    )
    assert run.returncode == status, run.stderr
    src = "gs://atari-replay-datasets/dqn/Pong/1/replay_logs"
    assert f"1000  {src}/$store$_observation_ckpt.50.gz" in run.stdout
    assert f"1000  {src}/add_count_ckpt.50.gz" in run.stdout
    if status == 0:
        names = [
            "$store$_observation", "$store$_action", "$store$_reward", "$store$_terminal",
            "add_count",
        ]  # fmt: skip
        copied = " ".join(f"{src}/{name}_ckpt.50.gz" for name in names)
        out = tmp_path / "data" / "Pong" / "1" / "replay_logs"
        assert log.read_text() == f"-m cp {copied} {out}/\n"
    else:
        assert "over the budget of 4999: not copying" in run.stderr
        assert not log.exists() and not (tmp_path / "data").exists()
