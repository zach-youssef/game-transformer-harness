"""RetroEnv: one game on a libretro core behind the gymnasium API (docs/architecture.md).

Each step is two calls into the extension: one runs `frameskip` frames with the action held
and returns only the last frame, converted to the observation size; the other reads the
game's RAM variables and evaluates the episode end. One core per process: parallel
environments are separate processes.
"""

from __future__ import annotations

import functools
import itertools
import os
import sys
import time
from collections.abc import Mapping
from pathlib import Path
from typing import Any, Literal

import gymnasium as gym
import numpy as np
import numpy.typing as npt

from gtharness import _native, cores
from gtharness.errors import ConfigError
from gtharness.record import Record
from gtharness.rewards import REQUIRED_RAM, Reward

Observation = npt.NDArray[np.uint8]
Action = int | np.integer | Mapping[str, Any]
ActionFormat = Literal["dict", "flat"]

GAMES_DIR = Path(__file__).resolve().parents[2] / "games"
"""The checkout's ``games/`` directory, the default for `RetroEnv.from_game`."""

INTERPOLATIONS = ("area", "bilinear")
"""The ``interpolation`` values the env (and the shard exporter) resize frames with."""
ORIENTATIONS = (0, 90, 270)
"""Clockwise rotations a game config, a shard export or a protocol can ask for."""

_LIBRARY_SUFFIX = {"win32": ".dll", "darwin": ".dylib"}.get(sys.platform, ".so")

_RECORDING_NUMBERS = itertools.count()
"""Numbers this process's recordings, so their names are unique even within one second."""


class RetroEnv(gym.Env[Observation, Action]):
    """A game (its `GameConfig`, docs/games.md) on a libretro core, as a gymnasium environment.

    Actions are the same in every game (docs/actions.md): a direction (0 neutral, 1-8
    clockwise from up) and the four button slots (shot, bomb, aux1, aux2) held for the whole
    step; a slot the game doesn't map does nothing. With ``action_format="dict"`` an action is
    ``{"direction": Discrete(9), "buttons": MultiBinary(4)}``; with ``"flat"`` it is one
    ``Discrete(144)`` integer, ``direction * 16 + buttons`` (bit i of buttons = slot i). Coin
    and start are never actions: only the reset script presses them.

    Observations are ``uint8[height, width, 3]`` RGB arrays, cropped and turned as the game
    config says, that the caller owns. Game RAM is never part of an observation: the game's
    RAM variables go to the reward function and to ``info["ram"]``, which is for logging and
    rewards and must not be fed to a policy.

    The reward is ``reward(previous_ram, current_ram)`` over each step (`gtharness.rewards`).
    ``terminated`` is the game's ``episode.terminate`` condition on the RAM variables after the
    step; ``truncated`` is always False (wrap the environment in a time limit for one). Every
    RAM variable's location is checked against the core's memory when the environment is
    made, so a read can fail mid-episode only on bytes that don't decode (a BCD variable
    holding a non-BCD byte), and `step` or `reset` then raises `HarnessError` naming it.

    With ``record``, every episode is recorded (docs/recording-format.md): from the end of
    its reset's script or state load, so the first observation's frames are in it, to the
    next reset or `close`. `recordings` lists the finished ones. When ``core_path`` is the
    build the checkout's ``cores.lock`` pins (by SHA-256, docs/cores.md), each recording keeps
    the core's lock entry. Starting a recording reloads
    the state, so the first frames match the replay rather than an unrecorded run (a core that
    repeats its frame shows a blank first observation).

    ``info["episode_frame"]`` counts the frames run since the last reset, including the
    reset's own (script and first observation). It is counted here rather than read from the
    core, because gymnasium's ``check_env`` requires the same info after the same seeded reset
    and actions, and the core's own frame count survives resets and savestate loads.

    Args:
        config: the game config.
        core_path: the core's dynamic library.
        content_path: the game's content file, or None for cores that run without one.
        system_dir: where the core looks for BIOS files.
        frameskip: frames run per step with the action held (>= 1); settable at runtime.
        obs_size: ``(height, width)`` of observations.
        action_format: "dict" or "flat" (above).
        reward: the reward function, for example `gtharness.rewards.score_delta`.
        reset_state: the savestate a ``reset = { state = ... }`` game starts from (the file's
            bytes, checked against its SHA-1 in game.toml); None for a game with a reset
            script.
        interpolation: "area" (averages, best for shrinking) or "bilinear".
        record: where and how to record episodes (`gtharness.record.Record`), or None.
        game_id: the game's directory under ``games/``, kept in recordings; required with
            ``record`` (`from_game` passes it).
    """

    metadata: dict[str, Any] = {"render_modes": []}  # noqa: RUF012 (gymnasium's convention)

    def __init__(
        self,
        config: _native.GameConfig,
        *,
        core_path: str | os.PathLike[str],
        content_path: str | os.PathLike[str] | None,
        system_dir: str | os.PathLike[str],
        frameskip: int,
        obs_size: tuple[int, int],
        action_format: ActionFormat,
        reward: Reward,
        reset_state: bytes | None,
        interpolation: str = "area",
        record: Record | None = None,
        game_id: str | None = None,
    ) -> None:
        super().__init__()
        if record is not None and not game_id:
            raise ValueError("record needs the game_id")
        if action_format not in ("dict", "flat"):
            raise ValueError(f'action_format must be "dict" or "flat", not {action_format!r}')
        if (reset_state is None) != (config.reset_state is None):
            needs = "needs" if config.reset_state else "takes no"
            raise ValueError(f"reset_state: the game {needs} a reset savestate")
        if reset_state is not None:
            config.check_reset_state(reset_state)
        # A built-in reward, also inside functools.partial, is found by identity (so an
        # unhashable callable works); any other wrapper skips this check.
        base = reward
        while isinstance(base, functools.partial):
            base = base.func
        required = next((names for known, names in REQUIRED_RAM.items() if known is base), ())
        missing = [name for name in required if name not in config.ram_names]
        if missing:
            raise ConfigError(f"{base.__name__} needs the RAM variables {missing}")
        self.frameskip = frameskip
        self._config = config
        self._reward = reward
        self._reset_state = reset_state
        self._reset_inputs = config.reset_inputs or []
        self._action_format = action_format
        self._joypad = config.joypad_table()
        self._button_bits = 1 << np.arange(4, dtype=np.uint32)
        self._episode_frames = 0
        self._ram: dict[str, int] = {}
        self._record = record
        self._game_id = game_id
        self._core_path = Path(core_path)
        self._core_lock: _native.CoresLock | Literal[False] | None = False
        self.recordings: list[Path] = []
        """The episodes recorded and finished so far, oldest first."""
        height, width = obs_size
        try:
            # Private, so the policy-facing API offers no core; gtharness.ramsearch, a
            # developer tool, drives it directly.
            self._core = _native.Core(
                core_path,
                content_path,
                system_dir,
                config.core_options,
                config.settle_frames,
                height,
                width,
                config.crop,
                config.orientation,
                interpolation,
            )
        except ConfigError as error:
            if not config.content_in_parent:
                raise
            raise ConfigError(
                f"{config.content_file} is in its parent's archive (content.in_parent), which"
                f" needs a core that loads content from its path: {error}"
            ) from error
        try:
            self._core.check_ram(config)
        except BaseException:
            self._core.close()
            raise
        self.observation_space = gym.spaces.Box(0, 255, (height, width, 3), np.uint8)
        if action_format == "flat":
            self.action_space: gym.spaces.Space[Any] = gym.spaces.Discrete(config.n_actions)
        else:
            self.action_space = gym.spaces.Dict(
                {
                    "direction": gym.spaces.Discrete(9),
                    "buttons": gym.spaces.MultiBinary(4),
                }
            )

    @classmethod
    def from_game(
        cls,
        game_id: str,
        data_dir: str | os.PathLike[str],
        *,
        frameskip: int,
        obs_size: tuple[int, int],
        action_format: ActionFormat,
        reward: Reward,
        rom_dir: str | os.PathLike[str] | None = None,
        core_path: str | os.PathLike[str] | None = None,
        games_dir: str | os.PathLike[str] = GAMES_DIR,
        interpolation: str = "area",
        record: Record | None = None,
    ) -> RetroEnv:
        """The game ``games_dir/<game_id>/game.toml``, with its files found (docs/roms.md).

        The content is found by SHA-1 under ``rom_dir`` (default: `GTH_ROM_DIR` or the user
        config file's ``rom_dir``), and its parents must be next to it; content ``in_parent``
        (a clone in a merged set) is found through its first parent. ``data_dir`` holds the
        rest: BIOS files in ``system/``, the core in ``cores/<core>_libretro.<ext>`` unless
        ``core_path`` is given, and a reset savestate in ``states/<game_id>/``. Raises
        `ConfigError` naming what is missing or doesn't match its hash.
        """
        data = Path(data_dir)
        config = _native.GameConfig.load(Path(games_dir) / game_id / "game.toml")
        content = find_content(config, game_id, rom_dir)
        system = data / "system"
        missing = config.missing_bios(system)
        if missing:
            raise ConfigError(f"{game_id}: BIOS files missing from {system}: {missing}")
        state = None
        if config.reset_state is not None:
            path = data / "states" / game_id / config.reset_state
            try:
                state = path.read_bytes()
            except OSError as error:
                raise ConfigError(f"{game_id}: cannot read the reset state: {error}") from error
        return cls(
            config,
            core_path=core_path or core_library(data, config.core),
            content_path=content,
            system_dir=system,
            frameskip=frameskip,
            obs_size=obs_size,
            action_format=action_format,
            reward=reward,
            reset_state=state,
            interpolation=interpolation,
            record=record,
            game_id=game_id,
        )

    @property
    def config(self) -> _native.GameConfig:
        return self._config

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
        """Whether `reset` starts from the state right after loading (True) or from the core's
        own soft reset, which may keep RAM between episodes (False)."""
        return self._core.resets_to_load_state

    def reset(
        self, *, seed: int | None = None, options: dict[str, Any] | None = None
    ) -> tuple[Observation, dict[str, Any]]:
        """Starts an episode: the game's reset savestate, or the state right after loading
        followed by the game's reset script; ``options["state"]`` (bytes from `save_state`)
        replaces both. Then runs `frameskip` frames with no button held and returns the last.

        Those frames give the first observation a real frame (a core draws only while it runs,
        and a savestate holds no frame) and keep steps on frameskip boundaries. All of it is
        deterministic, so episodes from the same start are identical. ``seed`` seeds
        ``np_random`` only; the emulation has no randomness of its own. A core that can't
        serialize at load time gets its own soft reset instead, which may keep RAM between
        episodes; `resets_to_load_state` says which one this core gets.
        """
        super().reset(seed=seed)
        self._finish_recording()
        self._episode_frames = 0
        state = (options or {}).get("state", self._reset_state)
        if state is None:
            self._core.reset()
            self._core.run_inputs(self._reset_inputs)
            self._episode_frames += sum(frames for _, frames in self._reset_inputs)
        else:
            self._core.unserialize(state)
        if self._record is not None:
            self._start_recording(self._record)
        observation = self._run(0)
        self._ram, _ = self._core.read_ram(self._config)
        return observation, self._info()

    def step(self, action: Action) -> tuple[Observation, float, bool, bool, dict[str, Any]]:
        """Holds ``action`` for `frameskip` frames; returns the last frame."""
        observation = self._run(self._joypad[self._flat(action)])
        ram, terminated = self._core.read_ram(self._config)
        reward = float(self._reward(self._ram, ram))
        self._ram = ram
        return observation, reward, terminated, False, self._info()

    def save_state(self) -> bytes:
        """The core's savestate, for ``reset(options={"state": ...})``."""
        return self._core.serialize()

    def memory(self, memory_id: int) -> bytes | None:
        """A copy of the core's memory ``RETRO_MEMORY_*``, or None if it exposes none. Like
        ``info["ram"]``, for tools and tests, never for a policy."""
        return self._core.memory(memory_id)

    def system_info(self) -> dict[str, str]:
        """The core's library name, version and content extensions."""
        return self._core.system_info()

    def close(self) -> None:
        """Finishes the recording in progress and unloads the core, so another environment
        can load one in this process."""
        try:
            if not self._core.closed:
                self._finish_recording()
        finally:
            self._core.close()
            super().close()

    def _flat(self, action: Action) -> int:
        """The flat action of ``action`` in this environment's format; ValueError if invalid."""
        if self._action_format == "flat":
            if isinstance(action, bool) or not isinstance(action, int | np.integer):
                raise ValueError(f"action must be an integer, not {action!r}")
            if not 0 <= action < len(self._joypad):
                raise ValueError(f"action {action} is out of range (0..{len(self._joypad)})")
            return int(action)
        if not isinstance(action, Mapping) or set(action) != {"direction", "buttons"}:
            raise ValueError(f'action must be {{"direction", "buttons"}}, not {action!r}')
        buttons = np.asarray(action["buttons"])
        if buttons.shape != self._button_bits.shape or not np.isin(buttons, (0, 1)).all():
            n = len(self._button_bits)
            raise ValueError(f"action buttons must be {n} values of 0 or 1, not {buttons!r}")
        direction = action["direction"]
        if (
            isinstance(direction, bool)
            or not isinstance(direction, int | np.integer)
            or not 0 <= direction < 9
        ):
            raise ValueError(f"action direction must be an integer in 0..9, not {direction!r}")
        return self._config.encode(
            int(direction), int(buttons.astype(np.uint32) @ self._button_bits)
        )

    def _run(self, buttons: int) -> Observation:
        observation = self._core.step(self._frameskip, buttons)
        self._episode_frames += self._frameskip
        return observation

    def _start_recording(self, record: Record) -> None:
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        number = next(_RECORDING_NUMBERS)
        name = f"{self._game_id}-{stamp}-{os.getpid()}-{number:04d}.gthrec"
        if self._core_lock is False:
            self._core_lock = cores.pinned_lock(cores.LOCK, self._config.core, self._core_path)
        self._core.start_recording(
            Path(record.directory) / name,
            self._config,
            self._game_id,
            self._core_lock,
            record.recorder,
            record.player,
            record.license,
            record.checks_every,
        )

    def _finish_recording(self) -> None:
        path = self._core.finish_recording()
        if path is not None:
            self.recordings.append(path)

    def _info(self) -> dict[str, Any]:
        return {"episode_frame": self._episode_frames, "ram": dict(self._ram)}


def find_content(
    config: _native.GameConfig, game_id: str, rom_dir: str | os.PathLike[str] | None
) -> Path:
    """``config``'s content file, found by SHA-1 under ``rom_dir`` (default: `GTH_ROM_DIR` or
    the user config file's ``rom_dir``), with its parent sets beside it (docs/roms.md). Raises
    `ConfigError` naming ``game_id`` and what is missing."""
    roms = Path(rom_dir) if rom_dir is not None else _native.rom_dir()
    if roms is None:
        raise ConfigError(
            f"{game_id}: no ROM directory: set GTH_ROM_DIR or rom_dir in the user config"
            f" ({_native.user_config_path()})"
        )
    content = config.find_content(roms)
    if content is None:
        if config.content_in_parent:
            raise ConfigError(
                f"{game_id}: the parent archive holding {config.content_file}"
                f" (content.in_parent) isn't in {roms}"
            )
        raise ConfigError(f"{game_id}: {config.content_file} (by SHA-1) isn't in {roms}")
    parents = config.missing_parents(content)
    if parents:
        raise ConfigError(f"{game_id}: parent sets missing from beside {content}: {parents}")
    return content


def core_library(data_dir: str | os.PathLike[str], core: str) -> Path:
    """Where a data directory keeps core ``core``: ``cores/<core>_libretro.<ext>``."""
    return Path(data_dir) / "cores" / f"{core}_libretro{_LIBRARY_SUFFIX}"
