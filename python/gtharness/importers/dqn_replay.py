"""DQN-Replay (Agarwal et al. 2020, gs://atari-replay-datasets) into training shards
(docs/recording-format.md, "Imported datasets", "DQN-Replay").

A DQN-Replay checkpoint is a saved Dopamine replay buffer: one gzipped ``.npy`` per stored
element (``$store$_observation_ckpt.N.gz`` and the action, reward and terminal ones) plus
``add_count_ckpt.N.gz``, in a ``<Game>/<run>/replay_logs`` directory. The buffer is circular:
its oldest transition is at ``add_count % capacity`` once it has wrapped. Each episode starts
with ``PADDING`` zero transitions (Dopamine pads its frame stacks with them), which aren't
samples. Decoding is gzip and numpy here, not Rust: the files are numpy's own format and the
work is one pass of decompression per file, not per-frame work; the shards are written by
`gtharness._native.write_dataset_shard`.

An import takes a contiguous slice of the buffer (in its logical, oldest-first order) and
writes one shard per episode piece, cut after each terminal transition.
"""

from __future__ import annotations

import argparse
import gzip
import json
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import IO, Any

import numpy as np
import numpy.typing as npt

from gtharness import _native
from gtharness.env import GAMES_DIR
from gtharness.errors import ConfigError, HarnessError
from gtharness.rewards import dataset_reward
from gtharness.shards import Shard

NAME = "DQN-Replay"
BUCKET = "gs://atari-replay-datasets/dqn"
PADDING = 3
"""Zero transitions before each episode: Dopamine's ``stack_size - 1`` with 4-frame stacks."""
FRAMESKIP = 4
FPS = 60.0
STICKY_ACTION_PROBABILITY = 0.25
RAM_NAMES = ("reward", "terminal")
ELEMENTS = ("observation", "action", "reward", "terminal")
_SKIP_BYTES = 1 << 24


@dataclass(frozen=True)
class Source:
    """One checkpoint of one run: ``replay_logs`` is ``.../<game>/<run>/replay_logs``."""

    replay_logs: Path
    checkpoint: int

    @property
    def game(self) -> str:
        return self.replay_logs.parent.parent.name

    @property
    def run(self) -> str:
        return self.replay_logs.parent.name

    @property
    def source_id(self) -> str:
        return f"dqn-replay-{self.game}-{self.run}-ckpt{self.checkpoint}"

    def path(self, name: str) -> Path:
        """The file of ``name``: a stored element (`ELEMENTS`) or ``add_count``."""
        stem = name if name == "add_count" else f"$store$_{name}"
        return self.replay_logs / f"{stem}_ckpt.{self.checkpoint}.gz"


def source(replay_logs: str | os.PathLike[str], checkpoint: int) -> Source:
    """The checkpoint ``checkpoint`` in ``replay_logs``, after checking its files exist.

    Raises:
        ConfigError: if the directory isn't a ``<game>/<run>/replay_logs`` one, or a file is
            missing."""
    path = Path(replay_logs).resolve()
    if path.name != "replay_logs" or not path.parent.parent.name:
        raise ConfigError(f"{path} isn't a DQN-Replay <game>/<run>/replay_logs directory")
    if checkpoint < 0:
        raise ConfigError(f"the checkpoint must be 0 or more, not {checkpoint}")
    result = Source(path, checkpoint)
    missing = [str(result.path(n)) for n in ("add_count", *ELEMENTS) if not result.path(n).exists()]
    if missing:
        raise ConfigError(f"missing DQN-Replay files: {missing}")
    return result


class _Npy:
    """A gzipped ``.npy`` read forward: its header, then rows."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self._file: IO[bytes] = gzip.open(path, "rb")  # noqa: SIM115 (closed by __exit__)
        try:
            version = np.lib.format.read_magic(self._file)
            if version == (1, 0):
                header = np.lib.format.read_array_header_1_0(self._file)
            elif version == (2, 0):
                header = np.lib.format.read_array_header_2_0(self._file)
            else:
                raise ConfigError(f"{path}: unsupported .npy version {version}")
        except (OSError, ValueError, EOFError) as error:
            self._file.close()
            raise ConfigError(f"{path}: not a gzipped .npy file ({error})") from error
        self.shape, fortran, self.dtype = header
        if fortran or self.dtype.hasobject:
            self._file.close()
            raise ConfigError(f"{path}: not a C-ordered array of plain values")
        # A 0-d array (add_count) reads as one row.
        self.n_rows = self.shape[0] if self.shape else 1
        self.row_bytes = int(np.prod(self.shape[1:], dtype=np.int64)) * self.dtype.itemsize
        self._row = 0

    def __enter__(self) -> _Npy:
        return self

    def __exit__(self, *_: object) -> None:
        self._file.close()

    def rows(self, start: int, stop: int) -> npt.NDArray[Any]:
        """Rows ``start`` to ``stop``; ``start`` can't be before the last read's end."""
        assert self._row <= start <= stop <= self.n_rows
        skip = (start - self._row) * self.row_bytes
        while skip:
            got = len(self._file.read(min(skip, _SKIP_BYTES)))
            if not got:
                break
            skip -= got
        size = (stop - start) * self.row_bytes
        data = self._file.read(size)
        if skip or len(data) != size:
            raise ConfigError(f"{self.path}: truncated ({self.n_rows} rows in the header)")
        self._row = stop
        return np.frombuffer(data, dtype=self.dtype).reshape((stop - start, *self.shape[1:]))


def _read_all(path: Path) -> npt.NDArray[Any]:
    with _Npy(path) as npy:
        return npy.rows(0, npy.n_rows).reshape(npy.shape)


def _read_logical(path: Path, physical: npt.NDArray[np.int64]) -> npt.NDArray[Any]:
    """The rows at ``physical`` (runs of consecutive indices, so one or two for a slice of a
    circular buffer), in that order, read in one forward pass."""
    assert len(physical)
    runs = np.split(physical, np.flatnonzero(np.diff(physical) != 1) + 1)
    parts: list[npt.NDArray[Any]] = [np.empty(0)] * len(runs)
    with _Npy(path) as npy:
        for k in sorted(range(len(runs)), key=lambda k: int(runs[k][0])):
            parts[k] = npy.rows(int(runs[k][0]), int(runs[k][-1]) + 1)
    return np.concatenate(parts)


@dataclass
class ImportResult:
    """What `import_slice` wrote, and the source's own values for the samples, to check."""

    shards: list[Path]
    samples: int
    padding: int
    """Padding transitions in the slice, dropped."""
    terminals: int
    """Terminal transitions in the slice: episodes the slice sees end."""
    reward_sum: float
    expected: dict[str, npt.NDArray[Any]] = field(repr=False)
    """The samples' frames, flat actions, rewards and episode frames, in shard order."""

    def summary(self) -> dict[str, Any]:
        return {
            "shards": [str(path) for path in self.shards],
            "samples": self.samples,
            "padding": self.padding,
            "terminals": self.terminals,
            "reward_sum": self.reward_sum,
        }


def import_slice(
    src: Source,
    out_dir: str | os.PathLike[str],
    *,
    game_id: str,
    start: int,
    count: int,
    chunk_samples: int,
    games_dir: str | os.PathLike[str] = GAMES_DIR,
) -> ImportResult:
    """Writes the transitions ``start`` to ``start + count`` of ``src``'s buffer (oldest
    first) as shards in ``out_dir``, ``<source_id>_<first transition:07>.npz``, one per
    episode piece, for the harness game ``game_id`` (an ``atari-<rom>`` id whose
    ``atari_minimal_actions`` the dataset's actions index).

    Raises:
        ConfigError: if the files don't fit together or the format, the slice isn't in the
            buffer, a reward isn't a whole number, an action isn't in the minimal set, a
            padding transition isn't zero, the slice is all padding, the game has no minimal
            set, or a shard already exists. All of these are checked before anything is
            written."""
    if count < 1 or start < 0:
        raise ConfigError(f"the slice needs start >= 0 and count >= 1, not {start}, {count}")
    config = _native.GameConfig.load(Path(games_dir) / game_id / "game.toml")
    minimal = config.atari_minimal_actions
    if not minimal:
        raise ConfigError(f"{game_id} has no atari_minimal_actions: not an Atari game")
    flat_of = np.array([config.from_atari(atari) for atari in minimal], dtype=np.int64)

    add_count = int(_read_all(src.path("add_count")))
    labels = {name: _read_all(src.path(name)) for name in ("action", "reward", "terminal")}
    with _Npy(src.path("observation")) as npy:
        capacity, obs_shape = npy.shape[0], npy.shape[1:]
    if len(obs_shape) != 2 or npy.dtype != np.uint8:
        raise ConfigError(f"{src.path('observation')}: not uint8 frames of one channel")
    for name, values in labels.items():
        if values.shape != (capacity,):
            raise ConfigError(f"{src.path(name)} has shape {values.shape}, not ({capacity},)")
    valid = min(add_count, capacity)
    wrapped = add_count > capacity
    if start + count > valid:
        raise ConfigError(f"the slice ends at {start + count}, past the buffer's {valid}")
    oldest = add_count % capacity if wrapped else 0
    physical = (oldest + np.arange(valid, dtype=np.int64)) % capacity
    action = labels["action"][physical].astype(np.int64)
    reward = labels["reward"][physical].astype(np.float64)
    terminal = labels["terminal"][physical].astype(bool)

    # Where each transition's episode starts: after the last terminal before it, or at the
    # buffer's start, which is an episode's (with its padding) unless the buffer wrapped.
    index = np.arange(valid, dtype=np.int64)
    after_terminal = np.where(terminal, index + 1, -1)
    last_start = np.maximum.accumulate(np.concatenate([[0 if not wrapped else -1], after_terminal]))
    episode_start = last_start[:-1]
    known = episode_start >= 0
    position = np.where(known, index - episode_start, index)
    padding = known & (position < PADDING)
    # One pass over the observations: the slice's, and a wrapped buffer's first PADDING,
    # since it can begin inside an overwritten episode's padding.
    head = min(PADDING, valid) if wrapped else 0
    if start <= head:
        logical = np.arange(max(head, start + count))
        read = _read_logical(src.path("observation"), physical[logical])
        frames = read[start : start + count]
    else:
        logical = np.concatenate([np.arange(head), np.arange(start, start + count)])
        read = _read_logical(src.path("observation"), physical[logical])
        frames = read[head:]
    if head:
        zero = ~read[:head].reshape(head, -1).any(axis=1)
        zero &= (action[:head] == 0) & (reward[:head] == 0) & ~terminal[:head]
        lead = int(np.argmin(zero)) if not zero.all() else head
        padding[:lead] |= ~known[:lead]
    # Agent steps since the episode's start (the buffer's when it's unknown): samples only.
    counts = np.concatenate([[0], np.cumsum(~padding)])
    step = counts[index] - counts[np.where(known, episode_start, 0)]
    episodes_before = np.cumsum(terminal) - terminal

    span = slice(start, start + count)
    keep = ~padding[span]
    bad = (frames[~keep].reshape(int((~keep).sum()), -1).any(axis=1)) | (
        (action[span][~keep] != 0) | (reward[span][~keep] != 0) | terminal[span][~keep]
    )
    if bad.any():
        at = start + int(np.flatnonzero(~keep)[np.argmax(bad)])
        raise ConfigError(f"transition {at} should be Dopamine's zero padding and isn't")
    if np.any(reward[span] != np.round(reward[span])):
        raise ConfigError("a reward isn't a whole number: the shard's reward column is int64")
    if np.any((action[span][keep] < 0) | (action[span][keep] >= len(minimal))):
        raise ConfigError(f"an action isn't in {game_id}'s minimal set of {len(minimal)}")

    samples = np.flatnonzero(keep) + start
    if not len(samples):
        raise ConfigError(f"transitions {start} to {start + count} are all padding")
    flat = flat_of[action[samples]]
    pieces = np.split(samples, np.flatnonzero(terminal[samples][:-1]) + 1)
    out = Path(out_dir)
    paths = [out / f"{src.source_id}_{int(piece[0]):07}.npz" for piece in pieces]
    existing = [str(path) for path in paths if path.exists()]
    if existing:
        raise ConfigError(f"shards already exist: {existing}")
    out.mkdir(parents=True, exist_ok=True)
    decoded = np.array([config.decode(int(a)) for a in flat_of], dtype=np.uint8).reshape(-1, 2)
    for piece, path in zip(pieces, paths, strict=True):
        rows = piece - start
        first = int(piece[0])
        meta = {
            "source_id": src.source_id,
            "episode_id": int(episodes_before[first]),
            "game_id": game_id,
            "fps": FPS,
            "frameskip": FRAMESKIP,
            "obs_size": list(obs_shape),
            "crop": None,
            "orientation": 0,
            "interpolation": "area",
            "maxpool": True,
            "grayscale": True,
            "ram_names": list(RAM_NAMES),
            "terminated": bool(terminal[piece[-1]]),
            "dataset": {
                "name": NAME,
                "url": f"{BUCKET}/{src.game}/{src.run}/replay_logs",
                "game": src.game,
                "run": src.run,
                "checkpoint": src.checkpoint,
                "first": first,
                "add_count": add_count,
                "capacity": capacity,
                "episode_start_known": bool(known[first]),
                "sticky_action_probability": STICKY_ACTION_PROBABILITY,
                "reward_clip": [-1, 1],
                "palette": "ale",
            },
        }
        _native.write_dataset_shard(
            path,
            json.dumps(meta),
            frames=np.ascontiguousarray(frames[rows][..., None]),
            direction=np.ascontiguousarray(decoded[action[piece], 0]),
            buttons=np.ascontiguousarray(decoded[action[piece], 1]),
            ram=np.stack([reward[piece], terminal[piece]], axis=1).astype(np.int64),
            episode_frame=(FRAMESKIP * step[piece]).astype(np.uint64),
            segment=np.zeros(len(piece), dtype=np.uint32),
            chunk_samples=chunk_samples,
        )
    return ImportResult(
        shards=paths,
        samples=len(samples),
        padding=int((~keep).sum()),
        terminals=int(terminal[span].sum()),
        reward_sum=float(reward[samples].sum()),
        expected={
            "frame": frames[samples - start][..., None],
            "action": flat,
            "reward": reward[samples],
            "episode_frame": (FRAMESKIP * step[samples]).astype(np.uint64),
            "terminal": terminal[samples],
        },
    )


def verify(result: ImportResult) -> dict[str, Any]:
    """Reads ``result``'s shards back through `gtharness.train.Windows` (one-sample windows,
    `dataset_reward`) and compares them with the source: sample count, frames, actions,
    rewards and their sum, episode frames, and episode ends against the source's terminals.
    ``ok`` is whether all of them match."""
    from gtharness.train import Windows

    got: dict[str, list[Any]] = {"frame": [], "action": [], "reward": [], "episode_frame": []}
    with Windows(
        result.shards,
        length=1,
        stride=1,
        reward=dataset_reward,
        action_format="flat",
        cache_chunks=2,
    ) as windows:
        for i in range(len(windows)):
            window = windows.window(i)
            for name, values in got.items():
                values.append(window[name][0])
        n_windows = len(windows)
    terminated = []
    for path in result.shards:
        with Shard(path) as shard:
            terminated.append(bool(shard.meta["terminated"]))
    expected = result.expected
    ends = np.flatnonzero(expected["terminal"])
    lengths = np.cumsum([0] + [_samples(path) for path in result.shards])
    checks = {
        "samples": n_windows == result.samples,
        **{
            name: n_windows == result.samples and np.array_equal(np.asarray(values), expected[name])
            for name, values in got.items()
        },
        # Each terminal transition is the last sample of a terminated shard, and only those.
        "episode_ends": np.array_equal(lengths[1:][np.asarray(terminated, dtype=bool)] - 1, ends),
    }
    return {
        "ok": all(checks.values()),
        "checks": checks,
        "windows": n_windows,
        "samples": result.samples,
        "reward_sum": float(np.sum(got["reward"])),
        "source_reward_sum": result.reward_sum,
        "source_terminals": result.terminals,
        "terminated_shards": sum(terminated),
    }


def _samples(path: Path) -> int:
    with Shard(path) as shard:
        return int(shard.meta["samples"])


def add_parser(commands: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    """Adds ``gth import-dqn-replay`` to the ``gth`` command line."""
    parser = commands.add_parser(
        "import-dqn-replay",
        help="write a slice of a DQN-Replay checkpoint as training shards",
        description="Writes the transitions START to START + COUNT (oldest first) of one "
        "DQN-Replay checkpoint as shards in OUT, one per episode piece (docs/"
        "recording-format.md, 'Imported datasets'), and prints a JSON summary. With --verify, "
        "reads them back through the training windows and compares them with the source. "
        "Exit status: 0 when written (and verified), 1 when the verification differs, 2 on a "
        "usage or config error.",
    )
    parser.add_argument(
        "replay_logs", type=Path, help="a downloaded <Game>/<run>/replay_logs directory"
    )
    parser.add_argument("--checkpoint", type=int, required=True, help="the files' ckpt number")
    parser.add_argument(
        "--game", required=True, help="the harness game id, e.g. atari-pong (its minimal set)"
    )
    parser.add_argument("--start", type=int, required=True, help="first transition, oldest = 0")
    parser.add_argument("--count", type=int, required=True, help="transitions in the slice")
    parser.add_argument("--out", type=Path, required=True, help="directory for the shards")
    parser.add_argument("--chunk-samples", type=int, required=True, help="samples per frame chunk")
    parser.add_argument("--verify", action="store_true", help="read the shards back and compare")
    parser.add_argument("--games-dir", type=Path, default=GAMES_DIR)


def main(args: argparse.Namespace) -> int:
    """Runs ``gth import-dqn-replay``; returns its exit status."""
    try:
        result = import_slice(
            source(args.replay_logs, args.checkpoint),
            args.out,
            game_id=args.game,
            start=args.start,
            count=args.count,
            chunk_samples=args.chunk_samples,
            games_dir=args.games_dir,
        )
        report = result.summary()
        if args.verify:
            report["verify"] = verify(result)
    except (HarnessError, ValueError, OverflowError) as error:
        print(f"gth import-dqn-replay: {error}", file=sys.stderr)
        return 2
    print(json.dumps(report, indent=2))
    return 1 if args.verify and not report["verify"]["ok"] else 0
