"""Reward functions for `RetroEnv`: callables on a game's RAM variables (docs/games.md).

A reward function takes the RAM variables before and after a step, by name, and returns the
step's reward. RAM variables are for rewards, episode ends and logging, never for the policy:
they reach the reward function and ``info["ram"]``, never the observation.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping

Reward = Callable[[Mapping[str, int], Mapping[str, int]], float]
"""``reward(previous, current) -> float``, on the RAM variables before and after a step."""


def score_delta(previous: Mapping[str, int], current: Mapping[str, int]) -> float:
    """The change of the ``score`` RAM variable (scaled, docs/games.md) over the step."""
    return float(current["score"] - previous["score"])


REQUIRED_RAM: dict[Reward, tuple[str, ...]] = {score_delta: ("score",)}
"""The RAM variables each built-in reward reads, checked when an environment is made.

`RetroEnv` finds the reward here by identity, looking through `functools.partial`; a reward
wrapped any other way isn't checked, and a missing variable raises `KeyError` at its first
step instead."""
