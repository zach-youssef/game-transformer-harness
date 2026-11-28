# Aggregate metrics

`gtharness.metrics` turns evaluation runs into the numbers papers report: normalised scores,
IQM, median, mean, optimality gap, probability of improvement, and stratified-bootstrap
confidence intervals on any of them. It follows Agarwal et al. 2021, the paper behind the
`rliable` library (`agarwal2021deep` in the research KB, cited below by claim id). It is numpy
only, with no native code, and bad input raises `ValueError` naming what is wrong.

## Runs

The aggregates and intervals treat the runs of a task as independent, identically distributed
realisations of the algorithm's score (`agarwal2021deep` A2): for a trained model that means
independent training runs. A run's score is the mean of its evaluation episodes
(`agarwal2021deep` Sec. 3, as in its Atari 100k study).

With one run per task (one checkpoint, as in MGDT-style evaluations), the stratified
bootstrap has nothing to resample and is refused (below).

## Normalised scores

`normalize(scores, low, high)` is the linear rescale

    normalised = (score - low) / (high - low)

so `low` maps to 0 and `high` to 1 (`agarwal2021deep` A1, C14). Both references are required
for every task: `normalize_tasks(scores, references)` refuses a task without its `(low, high)`
pair, and `low == high` is refused.

- Human-normalised score (HNS): `low` is the random agent's score and `high` the average
  human's, `100 x (agent - random) / (human - random)` in Mnih et al. 2015 (`mnih2015human`
  C8, Table 2); Agent57 also uses HNS and gives the 57-game table (`badia2020agent57` C9,
  C10). The two tables differ for some games (Alien's human 6875 in Mnih et al., 7127.7 in
  Agent57), so a protocol's comparison must name which table it uses.
- Our games: `low` is the random policy's mean under the same protocol, `high` the human
  reference (the HNS analogue).
- Any other pair (a DQN score, a min-max range) works the same way.

## Aggregates

Each takes a score matrix `x` of shape `[runs, tasks]` (N runs, M tasks), normally normalised
scores, or a batch `[..., runs, tasks]`, and returns one value per matrix. `run_matrix(scores,
tasks)` builds one from per-task runs; every task needs the same N.

| Function | Formula | Source |
|---|---|---|
| `mean(x)` | `mean_m( mean_n x[n, m] )`: the mean over tasks of each task's mean over runs | `agarwal2021deep` Sec. 2 |
| `median(x)` | `median_m( mean_n x[n, m] )` | `agarwal2021deep` Sec. 2; biased and high-variance with few runs (C1, C2) |
| `iqm(x)` | Sort all `n = N M` scores, drop the lowest and highest `floor(n / 4)`, take the mean of the rest (scipy's `trim_mean(x, 0.25)`, which rliable calls). For `n < 4` nothing is dropped. | `agarwal2021deep` C8 |
| `optimality_gap(x, *, threshold)` | `threshold - mean_{n,m} min(x[n, m], threshold)`: the mean shortfall below the threshold, 0 when every run reaches it | `agarwal2021deep` C9 (threshold 1, human level, for HNS) |
| `probability_of_improvement(x, y)` | `mean_m [ (1 / (N K)) sum_{i,j} ( 1[x_im > y_jm] + 0.5 1[x_im = y_jm] ) ]`: per task the Mann-Whitney U over N runs of x and K runs of y, ties counting half, then the mean over tasks. N and K may differ. | `agarwal2021deep` C9 |

The paper reports IQM with its interval as the main aggregate: it is robust to outlier runs
and has smaller intervals than the median (C4, C8); no one metric suffices and rankings can
change with the metric (C9, C12), so report several.

The `threshold` of the optimality gap is required, since it depends on the normalisation.

## Stratified-bootstrap intervals

`stratified_bootstrap_ci(statistic, *samples, resamples, rng, confidence)` returns an
`Interval` (`point`, `low`, `high`, `confidence`, `resamples`).

- Each sample is a score matrix `[runs, tasks]`; all have the same tasks. One sample for an
  aggregate (`iqm`, `mean`, ...), two for `probability_of_improvement`, and any callable that
  takes one `[b, runs, tasks]` batch per sample and returns `[b]` (bind the optimality gap's
  threshold with `functools.partial`).
- A resample draws, for each sample and each task independently, N runs with replacement from
  that task's N runs (`agarwal2021deep` Sec. 4.1, App. A.5): the tasks are the strata, so a
  resample keeps every task. Two samples are resampled independently of each other.
- The interval is the percentile interval: the `(1 - confidence) / 2` and
  `(1 + confidence) / 2` quantiles of the statistic over the resamples (numpy's linear
  interpolation). Percentile intervals cover better than basic or BCa ones with few runs
  (C6). `point` is the statistic on the data itself.
- `resamples`, `rng` (a `numpy.random.Generator`) and `confidence` are required, so a reported
  interval names them. rliable's defaults are 50,000 resamples for aggregates and 2,000 for
  probability of improvement (C11).
- The result depends only on the data, the rng's state (its bit generator's state, so a
  prior draw changes it and a restored `bit_generator.state` reproduces it) and `resamples`,
  not on the machine or on how the work is split: each sample draws from its own stream,
  seeded from four 63-bit draws of `rng` into a new bit generator of `rng`'s type, and
  resamples are evaluated in chunks bounded by the larger of each sample's scores and the
  pairwise comparisons across samples (runs of x times runs of y times tasks, what
  `probability_of_improvement` holds), about 32 MiB of float64 at a time.
- A sample with one run per task is refused: there is nothing to resample within a task, and a
  zero-width interval would look certain.

How far to trust the interval (`agarwal2021deep`): with 10 or more runs per task, percentile
intervals on IQM and median cover close to nominally; with 3 runs they are too narrow (C6).
Intervals on one task's mean need about 20-30 runs (C7).

A one-off cross-check against `rliable` itself runs in an experiment's own environment, not in
CI: the harness doesn't depend on it.
