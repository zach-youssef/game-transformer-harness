"""Recordings (docs/recording-format.md): what `RetroEnv` writes with ``record=Record(...)``,
and `Replayer`, which re-renders a recording's frames at any observation size."""

from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Literal

from gtharness._native import Replayer

__all__ = ["Record", "RecorderKind", "Replayer"]

RecorderKind = Literal["human", "policy", "scripted", "converted"]


@dataclass(frozen=True, kw_only=True)
class Record:
    """Where and how an environment records its episodes, one recording per episode.

    Every field is required, so who made a recording and under which license is never a
    silent default.

    Args:
        directory: where the ``<game_id>-<UTC time>-<pid>-<n>.gthrec`` recordings go (``n``
            numbers the process's recordings); created if missing.
        recorder: "human", "policy", "scripted" (random or scripted rollouts) or "converted".
        player: a person's handle, or the policy and checkpoint playing.
        license: the license the recordings are shared under, e.g. "CC-BY-NC-4.0".
        checks_every: frames between two desync checks: fewer find a desync closer to its
            cause and cost 24 bytes plus 9 per RAM variable each (before compression).
    """

    directory: str | os.PathLike[str]
    recorder: RecorderKind
    player: str
    license: str
    checks_every: int
