"""Evaluation protocols: every setting that changes an evaluation's scores, in one strict TOML file.

A protocol (docs/evaluation.md) fixes how a policy is evaluated: the games, frame skip, sticky
actions, observation, start, episode cap, episode and seed counts. Every field is required and
an unknown field is an error, so no default hides a choice. `EvalProtocol.expand` gives the
canonical TOML text and its SHA-1, which are stored with every result: two results are
comparable when their protocol SHA-1s are equal. Presets live in ``protocols/``.

Validation here is of the file alone. Checks that need a game (the game exists, its content
SHA-1 matches, it has ``score_var``, the backend supports a setting) happen when the protocol
is used.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal, NamedTuple

from gtharness.env import INTERPOLATIONS, ORIENTATIONS
from gtharness.errors import ConfigError

__all__ = ["PROTOCOLS_DIR", "EvalProtocol", "Expanded", "Obs", "Start", "load", "loads"]

PROTOCOLS_DIR = Path(__file__).resolve().parents[2] / "protocols"
"""The checkout's preset protocols."""

BACKENDS = ("libretro",)
ACTION_SETS = ("harness", "atari_full", "atari_minimal")
# A game id is its directory under games/: lowercase with hyphens (docs/games.md).
_GAME_ID = re.compile(r"[a-z0-9]+(-[a-z0-9]+)*")
_RAM_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_SHA1 = re.compile(r"[0-9a-f]{40}")

Crop = Literal["game"] | tuple[int, int, int, int]
Orientation = Literal["game"] | int


@dataclass(frozen=True)
class Obs:
    """What the policy sees: a frame converted as the env does (docs/evaluation.md)."""

    size: tuple[int, int]
    """``(height, width)`` of an observation."""
    crop: Crop
    """``"game"`` (the game config's crop) or ``(x, y, width, height)`` in core pixels."""
    orientation: Orientation
    """``"game"`` (the game config's) or clockwise degrees: 0, 90 or 270."""
    interpolation: str
    """One of `gtharness.env.INTERPOLATIONS`: ``"area"``, ``"bilinear"`` or ``"bicubic"``."""
    grayscale: bool


@dataclass(frozen=True)
class Start:
    """How an episode starts."""

    reset: str
    """``"game"`` (the game config's reset script or state) or a savestate's SHA-1."""
    noop_max: int
    """No no-ops when 0; otherwise 1 to ``noop_max`` no-op frames (uniform, from the env seed)
    after the reset, as baselines' ``NoopResetEnv`` draws them."""


@dataclass(frozen=True)
class EvalProtocol:
    """One evaluation protocol; fields in the order of docs/evaluation.md, which explains each."""

    backend: str
    games: tuple[str, ...]
    frameskip: int
    sticky_prob: float
    maxpool: bool
    obs: Obs
    frame_stack: int
    action_set: str
    start: Start
    max_episode_frames: int
    life_loss_terminal: bool
    reward_clip: str
    score_var: str
    episodes_per_seed: int
    env_seeds: tuple[int, ...]
    policy_seeds: tuple[int, ...]

    def expand(self) -> Expanded:
        """The canonical TOML text (no comments, fixed order and formatting) and its SHA-1.

        `loads` of the text gives back an equal protocol, and equal protocols give the same
        text, so the SHA-1 identifies the protocol whatever its file's comments or layout.
        """
        obs, start = self.obs, self.start
        top = [
            ("backend", self.backend),
            ("games", list(self.games)),
            ("frameskip", self.frameskip),
            ("sticky_prob", self.sticky_prob),
            ("maxpool", self.maxpool),
            ("frame_stack", self.frame_stack),
            ("action_set", self.action_set),
            ("max_episode_frames", self.max_episode_frames),
            ("life_loss_terminal", self.life_loss_terminal),
            ("reward_clip", self.reward_clip),
            ("score_var", self.score_var),
            ("episodes_per_seed", self.episodes_per_seed),
            ("env_seeds", list(self.env_seeds)),
            ("policy_seeds", list(self.policy_seeds)),
        ]
        obs_fields = [
            ("size", list(obs.size)),
            ("crop", obs.crop if isinstance(obs.crop, str) else list(obs.crop)),
            ("orientation", obs.orientation),
            ("interpolation", obs.interpolation),
            ("grayscale", obs.grayscale),
        ]
        start_fields = [("reset", start.reset), ("noop_max", start.noop_max)]
        lines = [f"{key} = {_toml_value(value)}" for key, value in top]
        for table, fields in (("obs", obs_fields), ("start", start_fields)):
            lines += ["", f"[{table}]"]
            lines += [f"{key} = {_toml_value(value)}" for key, value in fields]
        text = "\n".join(lines) + "\n"
        return Expanded(text, hashlib.sha1(text.encode("utf-8")).hexdigest())


class Expanded(NamedTuple):
    """A protocol's canonical TOML text and the SHA-1 of its UTF-8 bytes."""

    text: str
    sha1: str


def load(path: str | os.PathLike[str]) -> EvalProtocol:
    """Read and validate a protocol file; raises `ConfigError` naming the file and the field."""
    try:
        text = Path(path).read_text(encoding="utf-8")
    except OSError as error:
        raise ConfigError(f"{path}: cannot read the protocol: {error}") from error
    return loads(text, source=str(path))


def loads(text: str, source: str = "<protocol>") -> EvalProtocol:
    """Validate a protocol's TOML text; ``source`` names it in errors."""
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise ConfigError(f"{source}: not valid TOML: {error}") from error
    try:
        return _parse(data)
    except _FieldError as error:
        raise ConfigError(f"{source}: {error.field}: {error.message}") from None


class _FieldError(Exception):
    def __init__(self, field: str, message: str) -> None:
        super().__init__(f"{field}: {message}")
        self.field = field
        self.message = message


_TOP = (
    "backend",
    "games",
    "frameskip",
    "sticky_prob",
    "maxpool",
    "obs",
    "frame_stack",
    "action_set",
    "start",
    "max_episode_frames",
    "life_loss_terminal",
    "reward_clip",
    "score_var",
    "episodes_per_seed",
    "env_seeds",
    "policy_seeds",
)
_OBS = ("size", "crop", "orientation", "interpolation", "grayscale")
_START = ("reset", "noop_max")


def _fields(table: dict[str, Any], names: tuple[str, ...], prefix: str) -> dict[str, Any]:
    """The table's values for ``names``, after checking no field is missing or unknown."""
    for key in table:
        if key not in names:
            raise _FieldError(prefix + key, "unknown field")
    for name in names:
        if name not in table:
            raise _FieldError(prefix + name, "missing (every field is required)")
    return table


def _parse(data: dict[str, Any]) -> EvalProtocol:
    top = _fields(data, _TOP, "")
    obs = _fields(_table(top["obs"], "obs"), _OBS, "obs.")
    start = _fields(_table(top["start"], "start"), _START, "start.")
    parsed = _build(top, obs, start)
    if parsed.maxpool and parsed.frameskip < 2:
        # Pooling takes a step's last two frames; with one frame a step it would reach back
        # into the previous step.
        raise _FieldError("maxpool", "needs frameskip >= 2 (it pools a step's last two frames)")
    if parsed.start.noop_max >= parsed.max_episode_frames:
        raise _FieldError(
            "start.noop_max",
            f"must be below max_episode_frames ({parsed.max_episode_frames}), which counts"
            " the no-op frames",
        )
    return parsed


def _build(top: dict[str, Any], obs: dict[str, Any], start: dict[str, Any]) -> EvalProtocol:
    return EvalProtocol(
        backend=_choice(top["backend"], "backend", BACKENDS),
        games=_games(top["games"]),
        frameskip=_int(top["frameskip"], "frameskip", minimum=1),
        sticky_prob=_probability(top["sticky_prob"], "sticky_prob"),
        maxpool=_bool(top["maxpool"], "maxpool"),
        obs=Obs(
            size=_size(obs["size"]),
            crop=_crop(obs["crop"]),
            orientation=_orientation(obs["orientation"]),
            interpolation=_choice(obs["interpolation"], "obs.interpolation", INTERPOLATIONS),
            grayscale=_bool(obs["grayscale"], "obs.grayscale"),
        ),
        frame_stack=_int(top["frame_stack"], "frame_stack", minimum=1),
        action_set=_choice(top["action_set"], "action_set", ACTION_SETS),
        start=Start(
            reset=_reset(start["reset"]),
            noop_max=_int(start["noop_max"], "start.noop_max", minimum=0),
        ),
        max_episode_frames=_int(top["max_episode_frames"], "max_episode_frames", minimum=1),
        life_loss_terminal=_must(
            _bool(top["life_loss_terminal"], "life_loss_terminal"),
            "life_loss_terminal",
            False,
            "an evaluation episode ends at game over, never on a lost life",
        ),
        reward_clip=_must(
            _str(top["reward_clip"], "reward_clip"),
            "reward_clip",
            "none",
            "evaluation scores are the game's own, never clipped",
        ),
        score_var=_ram_name(top["score_var"], "score_var"),
        episodes_per_seed=_int(top["episodes_per_seed"], "episodes_per_seed", minimum=1),
        env_seeds=_seeds(top["env_seeds"], "env_seeds"),
        policy_seeds=_seeds(top["policy_seeds"], "policy_seeds"),
    )


def _table(value: Any, field: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise _FieldError(field, f"must be a table, not {_kind(value)}")
    return value


def _kind(value: Any) -> str:
    return f"{type(value).__name__} {value!r}"


def _int(value: Any, field: str, *, minimum: int) -> int:
    # bool is an int in Python, but `true` is no count in TOML.
    if not isinstance(value, int) or isinstance(value, bool):
        raise _FieldError(field, f"must be an integer, not {_kind(value)}")
    if value < minimum:
        raise _FieldError(field, f"must be at least {minimum}, not {value}")
    return value


def _bool(value: Any, field: str) -> bool:
    if not isinstance(value, bool):
        raise _FieldError(field, f"must be true or false, not {_kind(value)}")
    return value


def _str(value: Any, field: str) -> str:
    if not isinstance(value, str):
        raise _FieldError(field, f"must be a string, not {_kind(value)}")
    return value


def _choice(value: Any, field: str, choices: tuple[str, ...]) -> str:
    value = _str(value, field)
    if value not in choices:
        raise _FieldError(field, f"must be one of {', '.join(choices)}, not {value!r}")
    return value


def _must[T](value: T, field: str, required: T, why: str) -> T:
    if value != required:
        raise _FieldError(field, f"must be {_toml_value(required)}: {why}")
    return value


def _probability(value: Any, field: str) -> float:
    if not isinstance(value, int | float) or isinstance(value, bool):
        raise _FieldError(field, f"must be a number, not {_kind(value)}")
    # 1 would repeat the first action forever.
    if not 0 <= value < 1:
        raise _FieldError(field, f"must be in [0, 1), not {value}")
    # + 0.0 turns -0.0 into 0.0, which equals it, so equal protocols get the same text.
    return float(value) + 0.0


def _int_list(value: Any, field: str, *, minimum: int, length: int | None = None) -> list[int]:
    if not isinstance(value, list):
        raise _FieldError(field, f"must be an array, not {_kind(value)}")
    if length is not None and len(value) != length:
        raise _FieldError(field, f"must have {length} integers, not {len(value)}")
    for i, item in enumerate(value):
        _int(item, f"{field}[{i}]", minimum=minimum)
    return value


def _games(value: Any) -> tuple[str, ...]:
    if not isinstance(value, list) or not value:
        raise _FieldError("games", "must be a non-empty array of game ids")
    for i, item in enumerate(value):
        if not isinstance(item, str) or not _GAME_ID.fullmatch(item):
            raise _FieldError(f"games[{i}]", f"must be a game id (games/<id>), not {item!r}")
    _unique(value, "games")
    return tuple(value)


def _unique(values: list[Any], field: str) -> None:
    seen = set()
    for item in values:
        if item in seen:
            raise _FieldError(field, f"{item!r} is listed twice")
        seen.add(item)


def _size(value: Any) -> tuple[int, int]:
    height, width = _int_list(value, "obs.size", minimum=1, length=2)
    return height, width


def _crop(value: Any) -> Crop:
    if value == "game":
        return "game"
    if not isinstance(value, list):
        raise _FieldError("obs.crop", f'must be "game" or [x, y, width, height], not {value!r}')
    x, y, width, height = _int_list(value, "obs.crop", minimum=0, length=4)
    if width == 0 or height == 0:
        raise _FieldError("obs.crop", f"width and height must be at least 1, not {value}")
    return x, y, width, height


def _orientation(value: Any) -> Orientation:
    if value == "game" or (
        isinstance(value, int) and not isinstance(value, bool) and value in ORIENTATIONS
    ):
        return value
    raise _FieldError("obs.orientation", f'must be "game", 0, 90 or 270, not {value!r}')


def _reset(value: Any) -> str:
    value = _str(value, "start.reset")
    if value != "game" and not _SHA1.fullmatch(value):
        raise _FieldError(
            "start.reset", f'must be "game" or a savestate\'s lowercase SHA-1, not {value!r}'
        )
    return value


def _ram_name(value: Any, field: str) -> str:
    value = _str(value, field)
    if not _RAM_NAME.fullmatch(value):
        raise _FieldError(field, f"must be a RAM variable name from game.toml, not {value!r}")
    return value


def _seeds(value: Any, field: str) -> tuple[int, ...]:
    seeds = _int_list(value, field, minimum=0)
    if not seeds:
        raise _FieldError(field, "must list at least one seed")
    _unique(seeds, field)
    return tuple(seeds)


def _toml_value(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        return repr(value)  # shortest text that reads back as the same float
    if isinstance(value, str):
        return json.dumps(value)  # validated values are ASCII, where JSON and TOML agree
    if isinstance(value, list):
        return "[" + ", ".join(_toml_value(item) for item in value) + "]"
    raise TypeError(f"no TOML form for {value!r}")
