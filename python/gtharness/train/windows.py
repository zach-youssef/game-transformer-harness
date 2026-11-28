"""Sequence windows over training shards (docs/training.md): runs of consecutive samples within
one `segment`, each with its frames, actions, per-sample rewards, returns-to-go and timesteps,
for sequence models (frame stacks, decision transformers) and per-frame models alike.

numpy only: the PyTorch side wraps this (docs/training.md, "PyTorch"). Rewards come from the
shard's RAM variables through a reward function (`gtharness.rewards`), chosen at training time;
RAM itself is never in a window, as it is never a policy's input.
"""

from __future__ import annotations

import os
from collections import OrderedDict
from collections.abc import Callable, Iterator, Sequence
from typing import Literal

import numpy as np
import numpy.typing as npt

from gtharness.errors import ConfigError, HarnessError
from gtharness.rewards import Reward, required_ram
from gtharness.shards import Shard

ActionFormat = Literal["flat", "dict"]
_ACTION_FORMATS = ("flat", "dict")
_BUTTON_BITS = np.arange(4, dtype=np.uint8)
ChunkOf = Callable[[tuple[int, int]], npt.NDArray[np.uint8]]
"""Returns the decoded frames of ``(shard, chunk)``."""


def sample_rewards(shard: Shard, reward: Reward) -> npt.NDArray[np.float64]:
    """Each sample's reward: ``reward(ram[i], ram[i + 1])``, over the RAM variables at sample
    i's observation and at the next sample's, as `RetroEnv` rewards the step that answers an
    observation. A reward across a segment gap covers the dropped samples' frames too, so the
    shard's rewards still sum to the whole episode's. The last sample's reward is 0: the shard
    holds no RAM after its last sample's step.

    Raises:
        ConfigError: if ``reward`` is a built-in reward and the shard lacks a RAM variable it
            reads.
    """
    names = list(shard.meta["ram_names"])
    missing = [name for name in required_ram(reward) if name not in names]
    if missing:
        raise ConfigError(f"{shard.path}: the reward needs the RAM variables {missing}")
    rows = [dict(zip(names, row, strict=True)) for row in shard.ram.tolist()]
    rewards = np.zeros(len(rows), dtype=np.float64)
    for i in range(len(rows) - 1):
        rewards[i] = float(reward(rows[i], rows[i + 1]))
    return rewards


def returns_to_go(rewards: npt.NDArray[np.float64]) -> npt.NDArray[np.float64]:
    """``rtg[i] = rewards[i:].sum()``: the return from sample i to the shard's end, across
    segment gaps, as the episode goes on through them. That is the episode's return to come
    only when the shard ends at game over (``meta["terminated"]``); otherwise it is cut short
    where the recording stopped."""
    return np.cumsum(rewards[::-1])[::-1].copy()


def _read_only(array: np.ndarray) -> np.ndarray:
    """A read-only view of ``array``: windows are views of arrays later windows share."""
    view = array.view()
    view.setflags(write=False)
    return view


class Windows:
    """Every window of ``length`` consecutive samples within one segment of the shards at
    ``paths``, starting at each segment's first sample and every ``stride`` samples after it.
    A segment shorter than ``length`` gives no window, and the last ``stride - 1`` or fewer
    samples of a segment may be in none (docs/training.md, "Windows").

    A window (`window`, and each row of a `batches` batch) holds:

    - ``frame``: ``uint8[length, height, width, channels]``, the observations (3 channels,
      or 1 for gray shards);
    - the actions after them: ``action`` (``int64[length]``, ``direction * 16 + buttons``)
      with ``action_format="flat"``, or ``direction`` (``uint8[length]``) and ``buttons``
      (``uint8[length, 4]``, one 0/1 per slot) with ``"dict"``: `RetroEnv`'s action formats;
    - ``reward`` and ``return_to_go`` (``float64[length]``, `sample_rewards` and
      `returns_to_go` over the whole shard);
    - ``timestep`` (``int64[length]``): each sample's index in its shard, so agent steps
      since the shard's first sample (dropped samples aren't counted);
    - ``episode_frame`` (``uint64[length]``) and ``confidence`` (``float32[length]``), as in
      the shard;
    - ``shard`` and ``sample``: the index into ``paths`` and the first sample's index in it.

    A `window`'s arrays are read-only views of arrays that overlapping windows (and the cache)
    share, so writing to one raises instead of changing later windows: copy one to change
    it. A batch's arrays are new (stacked) and writable.

    Args:
        paths: the shards; all must have the same observation size.
        length: samples per window.
        stride: samples between the starts of consecutive windows in a segment: ``length``
            for windows that don't overlap, 1 for every window.
        reward: the reward function over the shards' RAM variables (`gtharness.rewards`).
        action_format: ``"flat"`` or ``"dict"``.
        cache_chunks: decoded frame chunks `window` keeps (most recently used): at least
            the chunks one window can span. `batches` decodes its own buffer instead.

    Opens every shard; close it (or use it as a context manager) to release them.
    """

    def __init__(
        self,
        paths: Sequence[str | os.PathLike[str]],
        *,
        length: int,
        stride: int,
        reward: Reward,
        action_format: ActionFormat,
        cache_chunks: int,
    ) -> None:
        if length < 1 or stride < 1:
            raise ValueError(f"length and stride must be at least 1, not {length}, {stride}")
        if action_format not in _ACTION_FORMATS:
            raise ValueError(
                f"action_format must be one of {_ACTION_FORMATS}, not {action_format!r}"
            )
        self.length, self.stride, self.action_format = length, stride, action_format
        self._shards: list[Shard] = []
        try:
            for path in paths:
                self._shards.append(Shard(path))
            shapes = {shard.obs_shape for shard in self._shards}
            if len(shapes) > 1:
                raise HarnessError(
                    f"the shards have different observation shapes (height, width, channels):"
                    f" {sorted(shapes)}"
                )
            span = max((self._span(shard) for shard in self._shards), default=1)
            if cache_chunks < span:
                raise ValueError(
                    f"cache_chunks must be at least {span}, the frame chunks a window of"
                    f" {length} samples can span, not {cache_chunks}"
                )
            self._cache_chunks = cache_chunks
            self._cache: OrderedDict[tuple[int, int], npt.NDArray[np.uint8]] = OrderedDict()
            self._labels = [self._shard_labels(shard, reward) for shard in self._shards]
            starts = [self._starts(shard.segment) for shard in self._shards]
            self._shard_index = np.concatenate(
                [np.full(len(s), i, dtype=np.int64) for i, s in enumerate(starts)]
                or [np.zeros(0, np.int64)]
            )
            self._start = np.concatenate(starts or [np.zeros(0, np.int64)])
        except BaseException:
            self.close()
            raise

    def __len__(self) -> int:
        return len(self._start)

    def __enter__(self) -> Windows:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    def close(self) -> None:
        for shard in self._shards:
            shard.close()

    @property
    def shards(self) -> Sequence[Shard]:
        """The open shards, in ``paths`` order."""
        return tuple(self._shards)

    def location(self, index: int) -> tuple[int, int]:
        """Window ``index``'s shard (an index into ``paths``) and first sample."""
        return int(self._shard_index[index]), int(self._start[index])

    def window(self, index: int) -> dict[str, np.ndarray]:
        """Window ``index`` (windows are numbered by shard, then by first sample), decoding
        the frame chunks it needs through the cache."""
        if not 0 <= index < len(self):
            raise IndexError(f"no window {index} of {len(self)}")
        return self._window(index, self._cached_chunk)

    def batches(
        self,
        batch_size: int,
        *,
        rng: np.random.Generator | None,
        buffer_chunks: int,
    ) -> Iterator[dict[str, np.ndarray]]:
        """Every window once, as batches of ``batch_size`` stacked windows (the last may be
        smaller). With ``rng``, windows are grouped by the frame chunk they start in, the
        groups visited in random order and the windows shuffled within ``buffer_chunks``
        groups at a time (more mixes better and holds more decoded frames); without, windows
        come in `window` order."""
        if batch_size < 1 or buffer_chunks < 1:
            raise ValueError("batch_size and buffer_chunks must be at least 1")
        chunk_samples = np.array([int(s.meta["chunk_samples"]) for s in self._shards] or [1])
        keys = self._shard_index * 2**32 + self._start // chunk_samples[self._shard_index]
        # Windows are sorted by shard and start, so each group is a contiguous run.
        _, group_starts = np.unique(keys, return_index=True)
        bounds = np.append(group_starts, len(self))
        groups = [np.arange(bounds[g], bounds[g + 1]) for g in range(len(group_starts))]
        if rng is not None:
            groups = [groups[g] for g in rng.permutation(len(groups))]
        decoded: dict[tuple[int, int], npt.NDArray[np.uint8]] = {}
        pending = np.zeros(0, dtype=np.int64)
        for at in range(0, len(groups), buffer_chunks):
            buffer = np.concatenate([pending, *groups[at : at + buffer_chunks]])
            if rng is not None:
                buffer = buffer[rng.permutation(len(buffer))]
            full = len(buffer) // batch_size * batch_size
            # Decode the chunks the buffer needs, keeping those already decoded.
            needed = {key for index in buffer for key in self._chunks_of(int(index))}
            decoded = {
                key: decoded[key] if key in decoded else self._decode(*key) for key in needed
            }
            for start in range(0, full, batch_size):
                yield self._stack(buffer[start : start + batch_size], decoded.__getitem__)
            pending = buffer[full:]
        if len(pending):
            yield self._stack(pending, decoded.__getitem__)

    def _span(self, shard: Shard) -> int:
        chunk_samples = int(shard.meta["chunk_samples"])
        return (self.length - 1 + chunk_samples - 1) // chunk_samples + 1

    def _starts(self, segment: npt.NDArray[np.uint32]) -> npt.NDArray[np.int64]:
        ends = np.flatnonzero(np.diff(segment.astype(np.int64))) + 1
        firsts = np.concatenate([[0], ends]) if len(segment) else np.zeros(0, np.int64)
        lasts = np.concatenate([ends, [len(segment)]]) if len(segment) else firsts
        runs = [
            np.arange(first, last - self.length + 1, self.stride, dtype=np.int64)
            for first, last in zip(firsts, lasts, strict=True)
        ]
        return np.concatenate(runs) if runs else np.zeros(0, np.int64)

    def _shard_labels(self, shard: Shard, reward: Reward) -> dict[str, np.ndarray]:
        rewards = sample_rewards(shard, reward)
        labels: dict[str, np.ndarray] = {
            "reward": rewards,
            "return_to_go": returns_to_go(rewards),
            "timestep": np.arange(len(shard), dtype=np.int64),
            "episode_frame": shard.episode_frame,
            "confidence": shard.confidence,
        }
        direction, buttons = shard.direction, shard.buttons
        if self.action_format == "flat":
            labels["action"] = direction.astype(np.int64) * 16 + buttons
        else:
            labels["direction"] = direction
            labels["buttons"] = (buttons[:, None] >> _BUTTON_BITS) & 1
        return {name: _read_only(values) for name, values in labels.items()}

    def _chunks_of(self, index: int) -> list[tuple[int, int]]:
        s, start = self.location(index)
        chunk_samples = int(self._shards[s].meta["chunk_samples"])
        first, last = start // chunk_samples, (start + self.length - 1) // chunk_samples
        return [(s, c) for c in range(first, last + 1)]

    def _decode(self, s: int, chunk: int) -> npt.NDArray[np.uint8]:
        return _read_only(self._shards[s].frames(chunk))

    def _cached_chunk(self, key: tuple[int, int]) -> npt.NDArray[np.uint8]:
        if key in self._cache:
            self._cache.move_to_end(key)
            return self._cache[key]
        frames = self._decode(*key)
        self._cache[key] = frames
        while len(self._cache) > self._cache_chunks:
            self._cache.popitem(last=False)
        return frames

    def _window(self, index: int, chunk_of: ChunkOf) -> dict[str, np.ndarray]:
        s, start = self.location(index)
        shard = self._shards[s]
        chunk_samples = int(shard.meta["chunk_samples"])
        parts = []
        for _, c in self._chunks_of(index):
            base = c * chunk_samples
            lo, hi = max(start, base) - base, min(start + self.length, base + chunk_samples) - base
            parts.append(chunk_of((s, c))[lo:hi])
        window = {"frame": _read_only(np.concatenate(parts)) if len(parts) > 1 else parts[0]}
        for name, values in self._labels[s].items():
            window[name] = values[start : start + self.length]
        window["shard"] = np.int64(s)
        window["sample"] = np.int64(start)
        return window

    def _stack(self, indices: npt.NDArray[np.int64], chunk_of: ChunkOf) -> dict[str, np.ndarray]:
        windows = [self._window(int(index), chunk_of) for index in indices]
        return {key: np.stack([w[key] for w in windows]) for key in windows[0]}
