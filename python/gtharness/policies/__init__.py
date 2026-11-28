"""Policies that `gtharness.evaluate` plays (docs/evaluation.md, "Policies").

A policy is any object with the attributes and methods of `Policy`. The runner makes one per
policy seed by calling the ``MODULE:NAME`` it is given as ``NAME(policy_seed=seed, **kwargs)``,
so a trained model can load the checkpoint of the training run that seed stands for (the
protocol's ``policy_seeds``). It checks each one's `ObsRequirements` against the protocol
before any episode and gives it only observations: never the game's RAM, which is for scores
and episode ends.
"""

from __future__ import annotations

import re
from collections.abc import Sequence
from dataclasses import dataclass
from typing import TYPE_CHECKING, NamedTuple, Protocol, runtime_checkable

import numpy as np
import numpy.typing as npt

if TYPE_CHECKING:
    from gtharness.protocol import EvalProtocol

__all__ = ["Action", "GameInfo", "ObsRequirements", "Policy", "check_identity"]

Action = int | np.integer | Sequence[int] | npt.NDArray[np.integer]
"""What `Policy.act` returns: one action, or a chunk of them, a non-empty 1-D sequence the
runner plays one per step before asking again. An action is in ``0..GameInfo.n_actions``: a
flat action (``direction * 16 + buttons``, docs/actions.md) under the ``harness`` action set,
an ALE action number under ``atari_full``, which the runner maps onto the pad."""

_SHA256 = re.compile(r"[0-9a-f]{64}")


class GameInfo(NamedTuple):
    """The game an episode plays, as a policy may know it: no RAM, no core."""

    game_id: str
    """The game's directory under ``games/``."""
    n_actions: int
    """The policy's actions are ``0..n_actions``: the flat actions under the ``harness``
    action set (docs/actions.md), ALE's 18 under ``atari_full``."""
    mapped_slots: tuple[str, ...]
    """The button slots that do something in this game (``shot``, ``bomb``, ``aux1``,
    ``aux2``); the others press nothing."""


@dataclass(frozen=True, kw_only=True)
class ObsRequirements:
    """What a policy needs from the protocol; the runner refuses a protocol that differs.

    Every field is required: None says, explicitly, that the policy takes any value.
    """

    size: tuple[int, int] | None
    """``(height, width)`` of an observation (the protocol's ``obs.size``)."""
    grayscale: bool | None
    """The protocol's ``obs.grayscale``."""
    maxpool: bool | None
    """The protocol's ``maxpool``."""
    frame_stack: int | None
    """Observations per `Policy.act` call (the protocol's ``frame_stack``)."""
    frameskip: int | None
    """Emulated frames per step (the protocol's ``frameskip``), the rate the policy acts at."""
    action_set: str | None
    """The protocol's ``action_set``."""

    def mismatches(self, protocol: EvalProtocol) -> list[str]:
        """``field: policy needs X, protocol has Y`` for each requirement ``protocol`` breaks."""
        has = {
            "obs.size": protocol.obs.size,
            "obs.grayscale": protocol.obs.grayscale,
            "maxpool": protocol.maxpool,
            "frame_stack": protocol.frame_stack,
            "frameskip": protocol.frameskip,
            "action_set": protocol.action_set,
        }
        needs = {
            "obs.size": self.size,
            "obs.grayscale": self.grayscale,
            "maxpool": self.maxpool,
            "frame_stack": self.frame_stack,
            "frameskip": self.frameskip,
            "action_set": self.action_set,
        }
        return [
            f"{field}: the policy needs {needs[field]!r}, the protocol has {has[field]!r}"
            for field in has
            if needs[field] is not None and needs[field] != has[field]
        ]


@runtime_checkable
class Policy(Protocol):
    """A policy the runner can evaluate.

    It is made as ``NAME(policy_seed=seed, **kwargs)``: ``policy_seed`` is the protocol's,
    the training run a trained model's weights come from (one checkpoint per seed), and
    `id` and `checkpoint_sha256` may depend on it. The runner calls ``reset`` at the start of
    every episode and ``act`` whenever it has no action left to play. Like `RetroEnv.reset`,
    ``reset(seed)`` seeds the policy's randomness (at the first episode of each pair of policy
    and env seed, a seed derived from both) and ``reset(None)`` lets it run on from the last
    episode, so a stochastic policy plays different episodes under one seed and the same ones
    when run again. ``reset`` also clears whatever the policy keeps within an episode (a
    recurrent state, a context).
    """

    id: str
    """What plays: a name and version (``random-v1``) or a model and its run; stored with the
    results and in recordings' ``player``."""
    checkpoint_sha256: str | None
    """The SHA-256 of the weights file the policy loaded, lowercase hex; None for a policy
    with no weights."""
    obs_requirements: ObsRequirements

    def reset(self, seed: int | None, game: GameInfo) -> None: ...

    def act(self, obs: npt.NDArray[np.uint8]) -> Action:
        """The next action, or a chunk of actions, for ``obs``: the protocol's last
        ``frame_stack`` observations, oldest first, as one ``uint8[frame_stack, height,
        width, channels]`` array the policy may keep (docs/evaluation.md, "Policies")."""
        ...


def check_identity(policy: object, what: str) -> Policy:
    """``policy`` as a `Policy`, after checking its attributes' types; raises TypeError naming
    ``what`` (its ``MODULE:NAME``) and the attribute that is wrong."""
    if not isinstance(policy, Policy):
        raise TypeError(
            f"{what} made {type(policy).__name__}, which isn't a Policy (it needs id,"
            " checkpoint_sha256, obs_requirements, reset and act)"
        )
    if not isinstance(policy.id, str) or not policy.id:
        raise TypeError(f"{what}: id must be a non-empty string, not {policy.id!r}")
    sha = policy.checkpoint_sha256
    if sha is not None and (not isinstance(sha, str) or not _SHA256.fullmatch(sha)):
        raise TypeError(
            f"{what}: checkpoint_sha256 must be None or 64 lowercase hex digits, not {sha!r}"
        )
    if not isinstance(policy.obs_requirements, ObsRequirements):
        raise TypeError(f"{what}: obs_requirements must be an ObsRequirements")
    return policy
