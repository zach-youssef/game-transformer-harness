"""Aggregate metrics (`gtharness.metrics`, docs/metrics.md) on hand-computed cases."""

from functools import partial

import numpy as np
import pytest

from gtharness import metrics
from gtharness.metrics import (
    iqm,
    mean,
    median,
    normalize,
    optimality_gap,
    probability_of_improvement,
    stratified_bootstrap_ci,
)


def ci(statistic, *samples, resamples=2000, seed=0, confidence=0.95):
    return stratified_bootstrap_ci(
        statistic,
        *samples,
        resamples=resamples,
        rng=np.random.default_rng(seed),
        confidence=confidence,
    )


# --- normalisation ---------------------------------------------------------------------------


def test_normalize_maps_low_to_0_and_high_to_1():
    np.testing.assert_allclose(normalize([1, 5, 9, 13], 1, 9), [0, 0.5, 1, 1.5])
    # A score below random is negative; Skiing-style negative references work as well.
    np.testing.assert_allclose(normalize([-17098.1, -4336.9], -17098.1, -4336.9), [0, 1])


@pytest.mark.parametrize(
    ("scores", "low", "high"), [([1.0], 2, 2), ([1.0], np.nan, 1), ([np.inf], 0, 1)]
)
def test_normalize_refuses_bad_input(scores, low, high):
    with pytest.raises(ValueError):
        normalize(scores, low, high)


def test_normalize_tasks_uses_each_tasks_reference_and_refuses_a_missing_one():
    out = metrics.normalize_tasks({"a": [0, 10], "b": [3]}, {"a": (0, 10), "b": (1, 5)})
    np.testing.assert_allclose(out["a"], [0, 1])
    np.testing.assert_allclose(out["b"], [0.5])
    with pytest.raises(ValueError, match="no \\(low, high\\) reference for b"):
        metrics.normalize_tasks({"a": [0], "b": [1]}, {"a": (0, 1)})


def test_run_matrix_stacks_columns_in_task_order():
    x = metrics.run_matrix({"b": [3, 4], "a": [1, 2]}, ["a", "b"])
    np.testing.assert_array_equal(x, [[1, 3], [2, 4]])
    with pytest.raises(ValueError, match="different run counts"):
        metrics.run_matrix({"a": [1, 2], "b": [3]}, ["a", "b"])
    with pytest.raises(ValueError, match="not the requested"):
        metrics.run_matrix({"a": [1]}, ["a", "b"])


# --- aggregates ------------------------------------------------------------------------------


def test_mean_and_median_are_over_per_task_run_means():
    x = [[1, 10, 3], [3, 20, 5]]  # per-task means 2, 15, 4
    assert mean(x) == pytest.approx(7.0)
    assert median(x) == pytest.approx(4.0)


def test_iqm_drops_the_outer_quarters_of_all_runs():
    x = np.arange(1, 9, dtype=float).reshape(4, 2)  # 1..8: drop 1, 2 and 7, 8
    assert iqm(x) == pytest.approx(4.5)
    x[3, 1] = 1000.0  # an outlier run changes nothing
    assert iqm(x) == pytest.approx(4.5)
    assert iqm([[1, 2], [3, 100]]) == pytest.approx(2.5)  # n = 4: drop one each side


def test_iqm_with_fewer_than_4_scores_is_the_mean():
    assert iqm([[7.0]]) == pytest.approx(7.0)  # one task, one run
    assert iqm([[1.0], [2.0], [6.0]]) == pytest.approx(3.0)  # one task, three runs


def test_iqm_of_ties():
    assert iqm(np.full((5, 3), 0.25)) == pytest.approx(0.25)
    assert iqm([[0, 0], [1, 1], [1, 1], [5, 5]]) == pytest.approx(1.0)


def test_one_task():
    x = [[0.5], [1.5], [1.0], [3.0]]
    assert mean(x) == pytest.approx(1.5)
    assert median(x) == pytest.approx(1.5)  # the one task's mean
    assert iqm(x) == pytest.approx(1.25)


def test_optimality_gap_is_the_mean_shortfall_below_the_threshold():
    x = [[0.5, 1.5], [1.0, 0.0]]  # min(x, 1) = 0.5, 1, 1, 0: mean 0.625
    assert optimality_gap(x, threshold=1.0) == pytest.approx(0.375)
    assert optimality_gap([[2.0, 3.0]], threshold=1.0) == pytest.approx(0.0)


def test_probability_of_improvement_counts_pairs_and_ties_half():
    assert probability_of_improvement([[1], [3]], [[2], [2]]) == pytest.approx(0.5)
    assert probability_of_improvement([[2]], [[2]]) == pytest.approx(0.5)
    assert probability_of_improvement([[3], [3]], [[3], [1]]) == pytest.approx(0.75)
    # Two tasks, x always better on the first and worse on the second; 3 runs against 2.
    x = [[5, 0], [6, 0], [7, 0]]
    y = [[1, 9], [2, 9]]
    assert probability_of_improvement(x, y) == pytest.approx(0.5)
    assert probability_of_improvement(y, x) == pytest.approx(0.5)
    with pytest.raises(ValueError, match="tasks"):
        probability_of_improvement([[1, 2]], [[1]])


def test_aggregates_batch_over_leading_axes():
    rng = np.random.default_rng(1)
    batch = rng.normal(size=(6, 5, 4))
    for f in (mean, median, iqm, partial(optimality_gap, threshold=0.5)):
        np.testing.assert_allclose(f(batch), [f(m) for m in batch])
    other = rng.normal(size=(6, 3, 4))
    np.testing.assert_allclose(
        probability_of_improvement(batch, other),
        [probability_of_improvement(a, b) for a, b in zip(batch, other, strict=True)],
    )


@pytest.mark.parametrize("bad", [[1.0, 2.0], [[]], [[np.nan]]])
def test_aggregates_refuse_bad_matrices(bad):
    with pytest.raises(ValueError):
        iqm(bad)


# --- stratified bootstrap --------------------------------------------------------------------


def test_bootstrap_of_constant_runs_is_the_point():
    interval = ci(iqm, np.full((3, 4), 0.7))
    assert (interval.point, interval.low, interval.high) == pytest.approx((0.7, 0.7, 0.7))
    assert (interval.confidence, interval.resamples) == (0.95, 2000)


def test_bootstrap_resamples_within_each_task():
    # Task a's runs are all 0 and task b's all 1: resampling within tasks keeps the mean at
    # 0.5 exactly; resampling runs across tasks would not.
    interval = ci(mean, [[0, 1], [0, 1], [0, 1]])
    assert (interval.low, interval.high) == pytest.approx((0.5, 0.5))

    columns = [{1, 2, 3}, {10, 20, 30}]

    def check(batch):
        for task, allowed in enumerate(columns):
            assert set(np.unique(batch[:, :, task])) <= allowed
        return batch.mean(axis=(-2, -1))

    ci(check, [[1, 10], [2, 20], [3, 30]], resamples=50)


def test_bootstrap_percentiles_on_two_runs():
    # One task, runs 0 and 1: a resample's mean is 0, 0.5 or 1 with probability 1/4, 1/2,
    # 1/4, so the 2.5th and 97.5th percentiles are 0 and 1, and the 40% interval (30th and
    # 70th percentiles, well inside the middle half) is 0.5.
    interval = ci(mean, [[0.0], [1.0]], resamples=4000)
    assert (interval.point, interval.low, interval.high) == pytest.approx((0.5, 0.0, 1.0))
    narrow = ci(mean, [[0.0], [1.0]], resamples=4000, confidence=0.4)
    assert (narrow.low, narrow.high) == pytest.approx((0.5, 0.5))


def test_bootstrap_is_deterministic_for_an_rng_whatever_the_chunk_size(monkeypatch):
    x = np.random.default_rng(2).normal(size=(5, 7))
    a = ci(iqm, x, seed=3)
    assert ci(iqm, x, seed=3) == a
    assert ci(iqm, x, seed=4) != a
    assert a.low < a.point < a.high
    monkeypatch.setattr(metrics, "_CHUNK_ELEMENTS", 3 * x.size + 1)  # 3 resamples per chunk
    assert ci(iqm, x, seed=3) == a
    # Two samples as well: each draws from its own stream.
    y = np.random.default_rng(5).normal(size=(4, 7))
    monkeypatch.undo()
    pair = ci(probability_of_improvement, x, y, resamples=200, seed=0, confidence=0.9)
    monkeypatch.setattr(metrics, "_CHUNK_ELEMENTS", 46)
    assert ci(probability_of_improvement, x, y, resamples=200, seed=0, confidence=0.9) == pair


def test_bootstrap_depends_on_the_rngs_state():
    x = np.random.default_rng(2).normal(size=(5, 7))

    def run(rng):
        return stratified_bootstrap_ci(iqm, x, resamples=200, rng=rng, confidence=0.9)

    fresh = run(np.random.default_rng(0))
    drawn = np.random.default_rng(0)
    drawn.normal(size=1000)
    state = drawn.bit_generator.state
    after_draw = run(drawn)
    assert after_draw != fresh  # a prior draw moves the stream
    restored = np.random.Generator(np.random.PCG64(99))
    restored.bit_generator.state = state
    assert run(restored) == after_draw  # numpy's save/restore idiom reproduces it
    # Any bit generator works, spawnable or not, and stays the caller's choice.
    legacy = np.random.MT19937()
    legacy._legacy_seeding(0)
    run(np.random.Generator(legacy))


def test_bootstrap_of_probability_of_improvement_resamples_both_samples():
    better = [[5.0, 6.0], [7.0, 8.0], [6.0, 9.0]]
    worse = [[1.0, 2.0], [0.0, 1.0]]
    interval = ci(probability_of_improvement, better, worse)
    assert (interval.point, interval.low, interval.high) == pytest.approx((1.0, 1.0, 1.0))
    mixed = ci(probability_of_improvement, [[1.0], [3.0]], [[2.0], [2.0]])
    assert mixed.point == pytest.approx(0.5)
    assert (mixed.low, mixed.high) == pytest.approx((0.0, 1.0))
    # x constant, y varying: the interval spans [0, 1] only if y is resampled too.
    other = ci(probability_of_improvement, [[2.0], [2.0]], [[1.0], [3.0]])
    assert other.point == pytest.approx(0.5)
    assert (other.low, other.high) == pytest.approx((0.0, 1.0))

    def different_draws(a, b):
        # Same-shaped samples, y = x + 10: shared row indices would keep that in every
        # resample (the first call is the data itself).
        if a.shape[0] > 1:
            assert not np.array_equal(a, b - 10.0)
        return probability_of_improvement(a, b)

    ci(different_draws, [[0.0], [1.0], [2.0]], [[10.0], [11.0], [12.0]], resamples=50)


def test_bootstrap_accepts_numpy_integer_resamples():
    interval = ci(iqm, [[1.0], [2.0]], resamples=np.int64(10))
    assert interval.resamples == 10 and type(interval.resamples) is int


@pytest.mark.parametrize(
    "kwargs",
    [
        {"resamples": 0},
        {"resamples": 10.0},
        {"resamples": True},
        {"resamples": np.bool_(True)},
        {"resamples": np.int64(0)},
        {"rng": 0},
        {"confidence": 1.0},
        {"confidence": 0.0},
    ],
)
def test_bootstrap_refuses_bad_parameters(kwargs):
    args = {"resamples": 10, "rng": np.random.default_rng(0), "confidence": 0.95} | kwargs
    with pytest.raises(ValueError):
        stratified_bootstrap_ci(iqm, [[1.0], [2.0]], **args)


def test_bootstrap_refuses_one_run_and_mismatched_tasks():
    with pytest.raises(ValueError, match="one run per task"):
        ci(iqm, [[1.0, 2.0, 3.0]])
    with pytest.raises(ValueError, match="task counts"):
        ci(probability_of_improvement, [[1.0], [2.0]], [[1.0, 2.0], [3.0, 4.0]])
    with pytest.raises(ValueError, match="statistic returned shape"):
        ci(lambda batch: batch.mean(axis=-2), [[1.0, 2.0], [3.0, 4.0]])


# --- run scores from results -----------------------------------------------------------------


def results(checkpoints):
    """A `check_results`-shaped dict: games g and h, policy seeds 0.. with `checkpoints`, two
    env seeds with one episode each; the score is 10 * seed + env seed (+100 on h)."""
    seeds = {
        str(seed): {"id": "m", "checkpoint_sha256": sha} for seed, sha in enumerate(checkpoints)
    }
    episodes = [
        {"game": game, "policy_seed": seed, "env_seed": env, "episode": 0, "score": score}
        for game, offset in (("g", 0), ("h", 100))
        for seed in range(len(checkpoints))
        for env in (0, 1)
        for score in [offset + 10 * seed + env]
    ]
    run = {"policy": {"seeds": seeds}, "games": {"g": {}, "h": {}}}
    return {"protocol": None, "run": run, "episodes": episodes}


def test_run_scores_average_each_policy_seeds_episodes():
    out = metrics.run_scores(results([None, None, None]), seeds_are="independent_runs")
    assert list(out) == ["g", "h"]
    np.testing.assert_allclose(out["g"], [0.5, 10.5, 20.5])
    np.testing.assert_allclose(out["h"], [100.5, 110.5, 120.5])


def test_run_scores_refuse_seeds_sharing_a_checkpoint_as_independent_runs():
    a, b = "a" * 64, "b" * 64
    metrics.run_scores(results([a, b]), seeds_are="independent_runs")
    shared = results([a, b, a])
    with pytest.raises(ValueError, match="policy seeds 0, 2 share checkpoint"):
        metrics.run_scores(shared, seeds_are="independent_runs")
    out = metrics.run_scores(shared, seeds_are="sampling_seeds")
    np.testing.assert_allclose(out["g"], [0.5, 10.5, 20.5])
    with pytest.raises(ValueError, match="seeds_are"):
        metrics.run_scores(shared, seeds_are="runs")  # type: ignore[arg-type]
