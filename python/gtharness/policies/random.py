"""The random policy: a uniformly random flat action every step (docs/evaluation.md,
"Policies"). It is the "random" of normalised scores, (agent - random) / (human - random).

Uniform over the 144 flat actions is uniform over what the actions do: every distinct press
(a direction and a set of mapped slots) appears 2^k times, k the unmapped slots, since an
unmapped slot presses nothing.
"""

from __future__ import annotations

import numpy as np
import numpy.typing as npt

from gtharness.policies import GameInfo, ObsRequirements

__all__ = ["RandomPolicy"]


class RandomPolicy:
    """Draws each step's flat action uniformly from ``0..n_actions``, with numpy's PCG64
    seeded by `reset`; it takes any observation, frame skip and stack."""

    id = "random-v1"
    """Bumped whenever the actions drawn for the same seed change."""
    checkpoint_sha256 = None
    obs_requirements = ObsRequirements(
        size=None,
        grayscale=None,
        maxpool=None,
        frame_stack=None,
        frameskip=None,
        # It draws flat harness actions; Atari's sets need their own action map (C13).
        action_set="harness",
    )

    def __init__(self, *, policy_seed: int) -> None:
        # No weights, so the seed only seeds the RNG until the first reset reseeds it.
        self._rng = np.random.default_rng(policy_seed)
        self._n_actions = 0

    def reset(self, seed: int | None, game: GameInfo) -> None:
        if seed is not None:
            self._rng = np.random.default_rng(seed)
        self._n_actions = game.n_actions

    def act(self, obs: npt.NDArray[np.uint8]) -> int:
        del obs
        return int(self._rng.integers(self._n_actions))
