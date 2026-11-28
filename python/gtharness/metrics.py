"""Aggregate metrics over evaluation runs (docs/metrics.md): normalised scores, IQM, median,
mean, optimality gap, probability of improvement and stratified-bootstrap confidence intervals,
following Agarwal et al. 2021 (rliable). numpy only.

Aggregates take a score matrix ``float[runs, tasks]`` (one row per run, one column per task),
or a batch of them ``float[..., runs, tasks]``, and return one value per matrix. Bad input
raises ValueError naming what is wrong.
"""

from __future__ import annotations

import math
import numbers
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass

import numpy as np
from numpy.typing import ArrayLike

# Elements a `stratified_bootstrap_ci` chunk holds at once (float64: 32 MiB): resampled scores,
# or for several samples the pairwise comparisons a statistic like probability_of_improvement
# makes. Intervals don't depend on it (each sample draws from its own stream).
_CHUNK_ELEMENTS = 1 << 22


def normalize(scores: ArrayLike, low: float, high: float) -> np.ndarray:
    """``(scores - low) / (high - low)``: 0 at `low` (random, say) and 1 at `high` (human)."""
    low, high = float(low), float(high)
    if not (np.isfinite(low) and np.isfinite(high)):
        raise ValueError(f"normalize: low {low} and high {high} must be finite")
    if low == high:
        raise ValueError(f"normalize: low and high are both {low}")
    x = np.asarray(scores, dtype=np.float64)
    _check_finite(x, "normalize: scores")
    return (x - low) / (high - low)


def normalize_tasks(
    scores: Mapping[str, ArrayLike], references: Mapping[str, tuple[float, float]]
) -> dict[str, np.ndarray]:
    """Normalises each task's scores with its own ``(low, high)`` from `references`; a task
    without one is refused, so no task is silently left unnormalised."""
    missing = [task for task in scores if task not in references]
    if missing:
        raise ValueError(f"normalize_tasks: no (low, high) reference for {', '.join(missing)}")
    return {task: normalize(x, *references[task]) for task, x in scores.items()}


def run_matrix(scores: Mapping[str, ArrayLike], tasks: Sequence[str]) -> np.ndarray:
    """Stacks per-task run scores (1-D, one per run) into ``float[runs, tasks]``, columns in
    `tasks`' order. Every task needs the same number of runs, and every task in `scores` must
    be in `tasks` (and the reverse)."""
    if len(set(tasks)) != len(tasks) or not tasks:
        raise ValueError("run_matrix: tasks must be non-empty and distinct")
    if set(scores) != set(tasks):
        raise ValueError(
            f"run_matrix: scores has tasks {sorted(scores)}, not the requested {sorted(tasks)}"
        )
    columns = [np.asarray(scores[task], dtype=np.float64) for task in tasks]
    for task, column in zip(tasks, columns, strict=True):
        if column.ndim != 1 or column.size == 0:
            raise ValueError(f"run_matrix: {task}: run scores must be 1-D and non-empty")
    counts = {task: column.size for task, column in zip(tasks, columns, strict=True)}
    if len(set(counts.values())) != 1:
        raise ValueError(f"run_matrix: tasks have different run counts {counts}")
    x = np.stack(columns, axis=1)
    _check_finite(x, "run_matrix")
    return x


def mean(x: ArrayLike) -> np.ndarray:
    """The mean over tasks of each task's mean over runs."""
    x = _batch(x, "mean")
    return x.mean(axis=(-2, -1))


def median(x: ArrayLike) -> np.ndarray:
    """The median over tasks of each task's mean over runs."""
    x = _batch(x, "median")
    return np.median(x.mean(axis=-2), axis=-1)


def iqm(x: ArrayLike) -> np.ndarray:
    """Interquartile mean: the mean of all runs of all tasks after dropping the lowest and
    highest ``floor(n / 4)`` of the ``n = runs * tasks`` scores (scipy's ``trim_mean(x,
    0.25)``, as rliable computes it). For ``n < 4`` that is the plain mean."""
    x = _batch(x, "iqm")
    flat = np.sort(x.reshape(*x.shape[:-2], -1), axis=-1)
    n = flat.shape[-1]
    cut = n // 4
    return flat[..., cut : n - cut].mean(axis=-1)


def optimality_gap(x: ArrayLike, *, threshold: float) -> np.ndarray:
    """The mean shortfall below `threshold` over all runs of all tasks:
    ``threshold - mean(min(x, threshold))``. Agarwal et al. use 1 (human level for HNS)."""
    x = _batch(x, "optimality_gap")
    threshold = float(threshold)
    if not np.isfinite(threshold):
        raise ValueError(f"optimality_gap: threshold {threshold} must be finite")
    return threshold - np.minimum(x, threshold).mean(axis=(-2, -1))


def probability_of_improvement(x: ArrayLike, y: ArrayLike) -> np.ndarray:
    """``P(X > Y)``: per task, the fraction of (run of x, run of y) pairs where x's run scores
    higher, ties counting half (the Mann-Whitney U statistic over ``N * K``); then the mean
    over tasks. `x` is ``[..., N, tasks]`` and `y` ``[..., K, tasks]``: run counts may differ,
    tasks may not."""
    x = _batch(x, "probability_of_improvement: x")
    y = _batch(y, "probability_of_improvement: y")
    if x.shape[-1] != y.shape[-1]:
        raise ValueError(
            f"probability_of_improvement: x has {x.shape[-1]} tasks, y has {y.shape[-1]}"
        )
    a = x[..., :, None, :]
    b = y[..., None, :, :]
    wins = (a > b).mean(axis=(-3, -2)) + 0.5 * (a == b).mean(axis=(-3, -2))
    return wins.mean(axis=-1)


@dataclass(frozen=True)
class Interval:
    """A statistic's value on the data and its percentile bootstrap interval."""

    point: float
    low: float
    high: float
    confidence: float
    resamples: int


def stratified_bootstrap_ci(
    statistic: Callable[..., np.ndarray],
    *samples: ArrayLike,
    resamples: int,
    rng: np.random.Generator,
    confidence: float,
) -> Interval:
    """Percentile stratified-bootstrap interval for ``statistic(*samples)``.

    Each sample is a score matrix ``[runs, tasks]`` (one algorithm's runs, say); all have the
    same tasks. A resample draws, independently for each sample and each task, as many runs
    as the task has, with replacement from that task's runs (Agarwal et al. 2021, Sec. 4.1).
    `statistic` takes one batch ``[b, runs, tasks]`` per sample and returns ``[b]`` (the
    aggregates above do), e.g. ``iqm`` or ``probability_of_improvement`` for two samples.

    `resamples` (Agarwal et al. use 50,000 for aggregates, 2,000 for probability of
    improvement), `rng` and `confidence` (0.95 for a 95% interval) are required so the
    reported interval states them. A task with one run has nothing to resample, so a sample
    with one run is refused rather than given a zero-width interval.
    """
    if isinstance(resamples, bool | np.bool_) or not isinstance(resamples, numbers.Integral):
        raise ValueError(f"stratified_bootstrap_ci: resamples {resamples!r} must be an int >= 1")
    resamples = int(resamples)
    if resamples < 1:
        raise ValueError(f"stratified_bootstrap_ci: resamples {resamples!r} must be an int >= 1")
    if not isinstance(rng, np.random.Generator):
        raise ValueError("stratified_bootstrap_ci: rng must be a numpy.random.Generator")
    confidence = float(confidence)
    if not 0.0 < confidence < 1.0:
        raise ValueError(f"stratified_bootstrap_ci: confidence {confidence} not in (0, 1)")
    if not samples:
        raise ValueError("stratified_bootstrap_ci: no samples")
    matrices = [_batch(s, f"stratified_bootstrap_ci: sample {i}") for i, s in enumerate(samples)]
    for i, m in enumerate(matrices):
        if m.ndim != 2:
            raise ValueError(f"stratified_bootstrap_ci: sample {i} must be [runs, tasks]")
        if m.shape[0] < 2:
            raise ValueError(
                f"stratified_bootstrap_ci: sample {i} has one run per task: nothing to resample"
            )
    if len({m.shape[1] for m in matrices}) != 1:
        raise ValueError("stratified_bootstrap_ci: samples have different task counts")

    point = _scalar_batch(statistic(*(m[None] for m in matrices)), 1)[0]
    # Per resample: each sample's scores, and every combination of one run from each sample
    # per task (what probability_of_improvement compares; the same as the scores for one).
    tasks = matrices[0].shape[1]
    per_resample = max(
        max(m.size for m in matrices), math.prod(m.shape[0] for m in matrices) * tasks
    )
    chunk = max(1, _CHUNK_ELEMENTS // per_resample)
    # One stream per sample, so each sample's draws are the same whatever the chunk size. The
    # streams are seeded from the rng's own draws (not rng.spawn, which ignores the stream
    # position), in the caller's bit generator type.
    streams = [_child(rng) for _ in matrices]
    values = np.empty(resamples)
    for start in range(0, resamples, chunk):
        b = min(chunk, resamples - start)
        batch = []
        for m, stream in zip(matrices, streams, strict=True):
            runs = m.shape[0]
            rows = stream.integers(0, runs, size=(b, runs, tasks))
            batch.append(m[rows, np.arange(tasks)])
        values[start : start + b] = _scalar_batch(statistic(*batch), b)
    tail = 100.0 * (1.0 - confidence) / 2.0
    low, high = np.percentile(values, [tail, 100.0 - tail])
    return Interval(
        point=float(point),
        low=float(low),
        high=float(high),
        confidence=confidence,
        resamples=resamples,
    )


def _child(rng: np.random.Generator) -> np.random.Generator:
    seed = np.random.SeedSequence(rng.integers(0, 2**63, size=4).tolist())
    try:
        return np.random.Generator(type(rng.bit_generator)(seed))
    except TypeError as error:
        raise ValueError(
            f"stratified_bootstrap_ci: can't seed a {type(rng.bit_generator).__name__}"
            f" from a SeedSequence: {error}"
        ) from error


def _batch(x: ArrayLike, what: str) -> np.ndarray:
    x = np.asarray(x, dtype=np.float64)
    if x.ndim < 2 or x.shape[-1] == 0 or x.shape[-2] == 0:
        raise ValueError(f"{what}: need a non-empty [..., runs, tasks] matrix, got {x.shape}")
    _check_finite(x, what)
    return x


def _check_finite(x: np.ndarray, what: str) -> None:
    if not np.all(np.isfinite(x)):
        raise ValueError(f"{what}: scores must be finite")


def _scalar_batch(values: ArrayLike, b: int) -> np.ndarray:
    values = np.asarray(values, dtype=np.float64)
    if values.shape != (b,):
        raise ValueError(
            f"stratified_bootstrap_ci: statistic returned shape {values.shape} for a batch of"
            f" {b}, not ({b},)"
        )
    return values
