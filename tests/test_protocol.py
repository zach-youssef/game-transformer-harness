"""Evaluation protocols (`gtharness.protocol`, docs/evaluation.md): parsing, validation, presets."""

import copy
import hashlib
import re
import tomllib
from dataclasses import replace
from typing import Any

import pytest

from gtharness import protocol
from gtharness.errors import ConfigError
from gtharness.protocol import PROTOCOLS_DIR, EvalProtocol, Obs, Start

# A valid protocol with every field, as parsed TOML; tests change one field at a time.
VALID: dict[str, Any] = {
    "backend": "libretro",
    "games": ["gunhed", "space-invaders"],
    "frameskip": 4,
    "sticky_prob": 0.25,
    "maxpool": True,
    "obs": {
        "size": [84, 96],
        "crop": [0, 8, 160, 200],
        "orientation": 90,
        "interpolation": "bilinear",
        "grayscale": True,
    },
    "frame_stack": 4,
    "action_set": "atari_full",
    "start": {"reset": "0107d93ff5d10325092d45e6bfd21e8130efeed7", "noop_max": 30},
    "max_episode_frames": 18000,
    "life_loss_terminal": False,
    "reward_clip": "none",
    "score_var": "score",
    "episodes_per_seed": 16,
    "env_seeds": [0, 7],
    "policy_seeds": [1, 2, 3],
}

EXPECTED = EvalProtocol(
    backend="libretro",
    games=("gunhed", "space-invaders"),
    frameskip=4,
    sticky_prob=0.25,
    maxpool=True,
    obs=Obs(
        size=(84, 96),
        crop=(0, 8, 160, 200),
        orientation=90,
        interpolation="bilinear",
        grayscale=True,
    ),
    frame_stack=4,
    action_set="atari_full",
    start=Start(reset="0107d93ff5d10325092d45e6bfd21e8130efeed7", noop_max=30),
    max_episode_frames=18000,
    life_loss_terminal=False,
    reward_clip="none",
    score_var="score",
    episodes_per_seed=16,
    env_seeds=(0, 7),
    policy_seeds=(1, 2, 3),
)

# Every field's dotted path.
FIELDS = [
    *(key for key in VALID if key not in ("obs", "start")),
    "obs",
    "start",
    *(f"obs.{key}" for key in VALID["obs"]),
    *(f"start.{key}" for key in VALID["start"]),
]

# Pinned so a change to a preset or to the canonical form is seen: a changed protocol is a
# new protocol, whose results don't compare with the old one's.
PRESET_SHA1 = {
    "ours-v1": "7c4e3f02dd52cb41daa736f7d91227f9cc178851",
    "machado2018": "50864e7534e14f0be8f514445373665686cba853",
    "mgdt2022": "9770da92b87c2a9a8fbf5b44c6d40516c2cb1097",
    "jowa2025": "d5067c237d3715ecf61cf9a10a21a739fed0588f",
}


def toml_text(data: dict[str, Any]) -> str:
    """``data`` as TOML, written by hand: the standard library reads TOML but can't write it."""

    def value(v: Any) -> str:
        if isinstance(v, bool):
            return "true" if v else "false"
        if isinstance(v, str):
            return f'"{v}"'
        if isinstance(v, list):
            return "[" + ", ".join(value(item) for item in v) + "]"
        return repr(v)

    tables = {k: v for k, v in data.items() if isinstance(v, dict)}
    lines = [f"{k} = {value(v)}" for k, v in data.items() if k not in tables]
    for name, table in tables.items():
        lines += [f"[{name}]", *(f"{k} = {value(v)}" for k, v in table.items())]
    return "\n".join(lines) + "\n"


def with_field(path: str, new: Any) -> dict[str, Any]:
    """`VALID` with the field at ``path`` set to ``new`` (`DELETE` removes it)."""
    data = copy.deepcopy(VALID)
    *parents, key = path.split(".")
    table = data
    for parent in parents:
        table = table[parent]
    if new is DELETE:
        del table[key]
    else:
        table[key] = new
    return data


DELETE = object()


def error(data: dict[str, Any]) -> str:
    with pytest.raises(ConfigError) as info:
        protocol.loads(toml_text(data), source="p.toml")
    return str(info.value)


def test_parses_every_field() -> None:
    assert protocol.loads(toml_text(VALID)) == EXPECTED


def test_game_crop_and_orientation() -> None:
    data = with_field("obs.crop", "game")
    data["obs"]["orientation"] = "game"
    data["start"]["reset"] = "game"
    parsed = protocol.loads(toml_text(data))
    assert parsed.obs.crop == "game"
    assert parsed.obs.orientation == "game"
    assert parsed.start.reset == "game"


def test_integer_sticky_prob_reads_as_float() -> None:
    parsed = protocol.loads(toml_text(with_field("sticky_prob", 0)))
    assert parsed.sticky_prob == 0.0
    assert isinstance(parsed.sticky_prob, float)
    assert "sticky_prob = 0.0\n" in parsed.expand().text


def test_negative_zero_sticky_prob_is_zero() -> None:
    parsed = protocol.loads(toml_text(with_field("sticky_prob", -0.0)))
    zero = protocol.loads(toml_text(with_field("sticky_prob", 0.0)))
    assert parsed == zero
    assert parsed.expand() == zero.expand()


def test_maxpool_needs_two_frames_a_step() -> None:
    data = with_field("frameskip", 1)
    assert error(data) == (
        "p.toml: maxpool: needs frameskip >= 2 (it pools a step's last two frames)"
    )
    data["maxpool"] = False
    assert protocol.loads(toml_text(data)).frameskip == 1


def test_noop_max_below_the_cap() -> None:
    data = with_field("max_episode_frames", 30)
    assert error(data).startswith("p.toml: start.noop_max: must be below max_episode_frames (30)")
    data["max_episode_frames"] = 31
    assert protocol.loads(toml_text(data)).max_episode_frames == 31


@pytest.mark.parametrize("field", FIELDS)
def test_missing_field(field: str) -> None:
    assert error(with_field(field, DELETE)) == (
        f"p.toml: {field}: missing (every field is required)"
    )


@pytest.mark.parametrize("table", ["", "obs.", "start."])
def test_unknown_field(table: str) -> None:
    assert error(with_field(f"{table}extra", 1)) == f"p.toml: {table}extra: unknown field"


BAD_VALUES = [
    ("backend", "ale", "must be one of libretro"),
    ("backend", 1, "must be a string"),
    ("games", [], "must be a non-empty array"),
    ("games", "gunhed", "must be a non-empty array"),
    ("games", ["Gunhed"], "games[0]: must be a game id"),
    ("games", ["space_invaders"], "games[0]: must be a game id"),
    ("games", ["gunhed", "../x"], "games[1]: must be a game id"),
    ("games", ["gunhed", 3], "games[1]: must be a game id"),
    ("games", ["gunhed", "gunhed"], "'gunhed' is listed twice"),
    ("frameskip", 0, "must be at least 1"),
    ("frameskip", 4.0, "must be an integer"),
    ("frameskip", True, "must be an integer"),
    ("sticky_prob", 1.0, "must be in [0, 1)"),
    ("sticky_prob", -0.1, "must be in [0, 1)"),
    ("sticky_prob", "0.25", "must be a number"),
    ("sticky_prob", False, "must be a number"),
    ("maxpool", 1, "must be true or false"),
    ("obs", 3, "must be a table"),
    ("obs.size", [84], "must have 2 integers"),
    ("obs.size", [84, 0], "obs.size[1]: must be at least 1"),
    ("obs.size", "84x84", "must be an array"),
    ("obs.crop", [0, 0, 10], "must have 4 integers"),
    ("obs.crop", [0, -1, 10, 10], "obs.crop[1]: must be at least 0"),
    ("obs.crop", [0, 0, 0, 10], "width and height must be at least 1"),
    ("obs.crop", "full", 'must be "game" or [x, y, width, height]'),
    ("obs.orientation", 180, 'must be "game", 0, 90 or 270'),
    ("obs.orientation", "90", 'must be "game", 0, 90 or 270'),
    ("obs.orientation", True, 'must be "game", 0, 90 or 270'),
    ("obs.interpolation", "cubic", "must be one of area, bilinear, bicubic"),
    ("obs.grayscale", "yes", "must be true or false"),
    ("frame_stack", 0, "must be at least 1"),
    ("action_set", "minimal", "must be one of harness, atari_full, atari_minimal"),
    ("start", [], "must be a table"),
    ("start.reset", "state.bin", 'must be "game" or a savestate\'s lowercase SHA-1'),
    ("start.reset", "0107D93FF5D10325092D45E6BFD21E8130EFEED7", "lowercase SHA-1"),
    ("start.noop_max", -1, "must be at least 0"),
    ("max_episode_frames", 0, "must be at least 1"),
    ("max_episode_frames", "none", "must be an integer"),
    ("life_loss_terminal", True, "must be false: an evaluation episode ends at game over"),
    ("life_loss_terminal", "false", "must be true or false"),
    ("reward_clip", "sign", 'must be "none": evaluation scores are the game\'s own'),
    ("reward_clip", False, "must be a string"),
    ("score_var", "", "must be a RAM variable name"),
    ("score_var", "score.hi", "must be a RAM variable name"),
    ("episodes_per_seed", 0, "must be at least 1"),
    ("env_seeds", [], "must list at least one seed"),
    ("env_seeds", [0, -1], "env_seeds[1]: must be at least 0"),
    ("env_seeds", [1, 1], "1 is listed twice"),
    ("env_seeds", 0, "must be an array"),
    ("policy_seeds", [], "must list at least one seed"),
    ("policy_seeds", [0, 0.5], "policy_seeds[1]: must be an integer"),
]


@pytest.mark.parametrize(("field", "value", "message"), BAD_VALUES)
def test_bad_value(field: str, value: Any, message: str) -> None:
    text = error(with_field(field, value))
    assert re.match(rf"p\.toml: {re.escape(field)}(\[\d+\])?: ", text), text
    assert message in text


def test_every_field_has_a_bad_value() -> None:
    tested = {field for field, _, _ in BAD_VALUES}
    assert tested == set(FIELDS)


def test_not_toml() -> None:
    with pytest.raises(ConfigError, match=r"^p\.toml: not valid TOML"):
        protocol.loads("frameskip = = 4", source="p.toml")


def test_load_names_the_file(tmp_path: Any) -> None:
    path = tmp_path / "bad.toml"
    path.write_text(toml_text(with_field("frameskip", 0)), encoding="utf-8")
    with pytest.raises(ConfigError, match=rf"^{re.escape(str(path))}: frameskip: "):
        protocol.load(path)
    with pytest.raises(ConfigError, match="cannot read the protocol"):
        protocol.load(tmp_path / "missing.toml")


CANONICAL = """\
backend = "libretro"
games = ["gunhed", "space-invaders"]
frameskip = 4
sticky_prob = 0.25
maxpool = true
frame_stack = 4
action_set = "atari_full"
max_episode_frames = 18000
life_loss_terminal = false
reward_clip = "none"
score_var = "score"
episodes_per_seed = 16
env_seeds = [0, 7]
policy_seeds = [1, 2, 3]

[obs]
size = [84, 96]
crop = [0, 8, 160, 200]
orientation = 90
interpolation = "bilinear"
grayscale = true

[start]
reset = "0107d93ff5d10325092d45e6bfd21e8130efeed7"
noop_max = 30
"""


def test_expand_canonical_text() -> None:
    expanded = EXPECTED.expand()
    assert expanded.text == CANONICAL
    assert expanded.sha1 == hashlib.sha1(CANONICAL.encode("utf-8")).hexdigest()


def test_expand_round_trips() -> None:
    expanded = EXPECTED.expand()
    assert protocol.loads(expanded.text) == EXPECTED
    assert protocol.loads(expanded.text).expand() == expanded
    # The canonical text is plain TOML whose tables come after the top-level fields.
    assert tomllib.loads(expanded.text) == VALID


def test_expand_ignores_comments_and_layout() -> None:
    reordered = {key: VALID[key] for key in reversed(list(VALID))}
    text = "# a comment\n\n" + toml_text(reordered)
    assert protocol.loads(text).expand() == EXPECTED.expand()


def test_any_change_changes_the_sha1() -> None:
    base = EXPECTED.expand().sha1
    changed = [
        replace(EXPECTED, frameskip=5),
        replace(EXPECTED, sticky_prob=0.0),
        replace(EXPECTED, games=("space-invaders", "gunhed")),  # order is part of it
        replace(EXPECTED, obs=replace(EXPECTED.obs, grayscale=False)),
        replace(EXPECTED, start=replace(EXPECTED.start, noop_max=0)),
        replace(EXPECTED, policy_seeds=(1, 2)),
    ]
    assert len({base, *(p.expand().sha1 for p in changed)}) == len(changed) + 1


def test_presets_directory() -> None:
    assert sorted(path.stem for path in PROTOCOLS_DIR.glob("*.toml")) == sorted(PRESET_SHA1)


@pytest.mark.parametrize("name", sorted(PRESET_SHA1))
def test_preset(name: str) -> None:
    preset = protocol.load(PROTOCOLS_DIR / f"{name}.toml")
    expanded = preset.expand()
    assert protocol.loads(expanded.text) == preset
    assert expanded.sha1 == PRESET_SHA1[name], expanded.text
    assert preset.life_loss_terminal is False
    assert preset.reward_clip == "none"
