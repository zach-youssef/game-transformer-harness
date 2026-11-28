"""Reward functions for `RetroEnv`: callables on a game's RAM variables (docs/games.md).

A reward function takes the RAM variables before and after a step, by name, and returns the
step's reward. RAM variables are for rewards, episode ends and logging, never for the policy:
they reach the reward function and ``info["ram"]``, never the observation.
"""

from __future__ import annotations

import functools
from collections.abc import Callable, Mapping

Reward = Callable[[Mapping[str, int], Mapping[str, int]], float]
"""``reward(previous, current) -> float``, on the RAM variables before and after a step."""


def score_delta(previous: Mapping[str, int], current: Mapping[str, int]) -> float:
    """The change of the ``score`` RAM variable (scaled, docs/games.md) over the step."""
    return float(current["score"] - previous["score"])


def dataset_reward(previous: Mapping[str, int], current: Mapping[str, int]) -> float:
    """The step's reward as an imported dataset recorded it (docs/recording-format.md,
    "Imported datasets"): the ``reward`` column, stored with the observation the step
    answers, so it reads ``previous`` alone. Only shards have it: a game's RAM has no
    ``reward`` variable, so `RetroEnv` refuses it."""
    return float(previous["reward"])


REQUIRED_RAM: dict[Reward, tuple[str, ...]] = {
    score_delta: ("score",),
    dataset_reward: ("reward",),
}
"""The RAM variables each built-in reward reads, checked when an environment is made.

`RetroEnv` finds the reward here by identity, looking through `functools.partial`; a reward
wrapped any other way isn't checked, and a missing variable raises `KeyError` at its first
step instead."""


PREVIOUS_ONLY: tuple[Reward, ...] = (dataset_reward,)
"""The built-in rewards that read only the RAM before the step, so the training windows give
a shard's last sample its reward too (docs/training.md, "Rewards and returns-to-go")."""


def _base(reward: Reward) -> Reward:
    while isinstance(reward, functools.partial):
        reward = reward.func
    return reward


def required_ram(reward: Reward) -> tuple[str, ...]:
    """The RAM variables ``reward`` reads, from `REQUIRED_RAM`, looking through
    `functools.partial`; ``()`` for a reward it doesn't list."""
    base = _base(reward)
    return next((names for known, names in REQUIRED_RAM.items() if known is base), ())


def reads_previous_only(reward: Reward) -> bool:
    """Whether ``reward`` is in `PREVIOUS_ONLY`, looking through `functools.partial`."""
    base = _base(reward)
    return any(known is base for known in PREVIOUS_ONLY)
