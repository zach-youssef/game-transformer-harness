"""RetroEnv: one libretro core behind the gymnasium API (docs/architecture.md, "Frame lifecycle").

Each step is one call into the extension, which runs `frameskip` frames with the action held
and returns only the last frame, converted to the observation size. One core per process:
parallel environments are separate processes.
"""

from __future__ import annotations

import os
from typing import Any

import gymnasium as gym
import numpy as np
import numpy.typing as npt

from gtharness import _native

Observation = npt.NDArray[np.uint8]
Action = npt.NDArray[np.integer]

BUTTONS = 16
"""Joypad buttons, ``RETRO_DEVICE_ID_JOYPAD_B`` (0) to ``RETRO_DEVICE_ID_JOYPAD_R3`` (15)."""

_BUTTON_BITS = 1 << np.arange(BUTTONS, dtype=np.uint32)


class RetroEnv(gym.Env[Observation, Action]):
    """A libretro core as a gymnasium environment.

    The action is the port 0 joypad as ``MultiBinary(16)``: ``action[i] == 1`` holds button
    ``RETRO_DEVICE_ID_JOYPAD_*`` number ``i`` for the whole step (a typed action space per game
    comes later). Observations are ``uint8[height, width, 3]`` RGB arrays that the caller owns.
    The reward is always 0 and episodes never end: games define those later (from RAM
    variables), on top of this environment.

    ``info["episode_frame"]`` counts the frames run since the last reset, including the reset's
    own. It is counted here rather than read from the core, because gymnasium's ``check_env``
    requires the same info after the same seeded reset and actions, and the core's own frame
    count survives resets and savestate loads.

    Args:
        core_path: the core's dynamic library.
        content_path: the ROM or other content, or None for cores that run without one.
        system_dir: where the core looks for BIOS files.
        frameskip: frames run per step with the action held (>= 1); settable at runtime.
        obs_size: ``(height, width)`` of observations.
        rotate: clockwise degrees (0, 90 or 270) to turn the frame, for vertical games.
        crop: ``(x, y, width, height)`` of the core's frame to keep, before rotation, or None
            for the whole frame.
        interpolation: "area" (averages, best for shrinking) or "bilinear".
    """

    metadata: dict[str, Any] = {"render_modes": []}  # noqa: RUF012 (gymnasium's convention)

    def __init__(
        self,
        core_path: str | os.PathLike[str],
        content_path: str | os.PathLike[str] | None,
        system_dir: str | os.PathLike[str],
        *,
        frameskip: int,
        obs_size: tuple[int, int],
        rotate: int,
        crop: tuple[int, int, int, int] | None = None,
        interpolation: str = "area",
    ) -> None:
        super().__init__()
        self.frameskip = frameskip
        self._episode_frames = 0
        height, width = obs_size
        self._core = _native.Core(
            core_path, content_path, system_dir, {}, height, width, crop, rotate, interpolation
        )
        self.observation_space = gym.spaces.Box(0, 255, (height, width, 3), np.uint8)
        self.action_space = gym.spaces.MultiBinary(BUTTONS)

    @property
    def frameskip(self) -> int:
        """Frames run per step; change it between steps (for example per curriculum stage)."""
        return self._frameskip

    @frameskip.setter
    def frameskip(self, frames: int) -> None:
        if isinstance(frames, bool) or not isinstance(frames, int | np.integer) or frames < 1:
            raise ValueError(f"frameskip must be an integer >= 1, not {frames!r}")
        self._frameskip = int(frames)

    @property
    def resets_to_load_state(self) -> bool:
        """Whether `reset` restores the state right after loading (True) or falls back to the
        core's own soft reset, which may keep RAM between episodes (False)."""
        return self._core.resets_to_load_state

    def reset(
        self, *, seed: int | None = None, options: dict[str, Any] | None = None
    ) -> tuple[Observation, dict[str, Any]]:
        """Returns to the state right after loading, or loads ``options["state"]`` (bytes from
        `save_state`), then runs `frameskip` frames with no button held and returns the last.

        Those frames give the first observation a real frame (a core draws only while it runs,
        and a savestate holds no frame) and keep steps on frameskip boundaries. They are
        deterministic, so episodes from the same start are identical. ``seed`` seeds
        ``np_random`` only; the emulation has no randomness of its own. A core that can't
        serialize at load time gets its own soft reset instead, which may keep RAM between
        episodes; `resets_to_load_state` says which one this core gets.
        """
        super().reset(seed=seed)
        state = (options or {}).get("state")
        if state is None:
            self._core.reset()
        else:
            self._core.unserialize(state)
        self._episode_frames = 0
        return self._run(0), self._info()

    def step(self, action: Action) -> tuple[Observation, float, bool, bool, dict[str, Any]]:
        """Holds the buttons in ``action`` for `frameskip` frames; returns the last frame."""
        pressed = np.asarray(action)
        if pressed.shape != (BUTTONS,) or not np.isin(pressed, (0, 1)).all():
            raise ValueError(f"action must be {BUTTONS} values of 0 or 1, not {action!r}")
        return (
            self._run(int(pressed.astype(np.uint32) @ _BUTTON_BITS)),
            0.0,
            False,
            False,
            self._info(),
        )

    def save_state(self) -> bytes:
        """The core's savestate, for ``reset(options={"state": ...})``."""
        return self._core.serialize()

    def memory(self, memory_id: int) -> bytes | None:
        """A copy of the core's memory ``RETRO_MEMORY_*``, or None if it exposes none."""
        return self._core.memory(memory_id)

    def system_info(self) -> dict[str, str]:
        """The core's library name, version and content extensions."""
        return self._core.system_info()

    def close(self) -> None:
        """Unloads the core, so another environment can load one in this process."""
        self._core.close()
        super().close()

    def _run(self, buttons: int) -> Observation:
        observation = self._core.step(self._frameskip, buttons)
        self._episode_frames += self._frameskip
        return observation

    def _info(self) -> dict[str, Any]:
        return {"episode_frame": self._episode_frames}
