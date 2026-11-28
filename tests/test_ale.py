"""The Atari games on the ALE core (docs/games.md, "Atari (ALE core)"): every config follows
the shared recipe, and the Atari presets name exactly those games."""

import tomllib
from pathlib import Path

import pytest

from gtharness import _native
from gtharness.env import GAMES_DIR
from gtharness.protocol import load

ATARI = sorted(path.name for path in GAMES_DIR.glob("atari-*"))
PRESETS = Path(__file__).resolve().parent.parent / "protocols"


def test_the_atari_presets_play_the_configured_games():
    assert len(ATARI) == 12
    machado = load(PRESETS / "machado2018.toml")
    assert sorted(machado.games) == ATARI, "machado2018 plays the whole pilot set"
    for name in ("mgdt2022", "jowa2025"):
        assert set(load(PRESETS / f"{name}.toml").games) < set(ATARI), name


@pytest.mark.parametrize("game_id", ATARI)
def test_atari_configs_follow_the_shared_recipe(game_id: str):
    path = GAMES_DIR / game_id / "game.toml"
    raw = tomllib.loads(path.read_text(encoding="utf-8"))
    config = _native.GameConfig.load(path)
    assert config.core == "ale" and config.reset_inputs == [] and config.reset_state is None
    assert config.mapped_slots == ["shot"] and raw["buttons"] == {"b": "shot"}
    assert config.ram_names == ["game_over", "lives", "score"]
    assert raw["ram"]["score"] == {"address": 0x10000, "size": 4, "encoding": "sle"}
    assert raw["ram"]["lives"] == {"address": 0x10008, "size": 4, "encoding": "le"}
    assert raw["ram"]["game_over"] == {"address": 0x1000C, "size": 1, "encoding": "le"}
    assert raw["episode"] == {"terminate": "game_over != 0"}
    assert config.terminated({"game_over": 1, "lives": 3, "score": 0})
    assert not config.terminated({"game_over": 0, "lives": 0, "score": -21}), "life loss"
    assert config.core_options == {"ale_mode": "default", "ale_difficulty": "default"}
    minimal = config.atari_minimal_actions
    assert 0 in minimal and len(minimal) >= 4, "every minimal set has NOOP and some moves"
    content = raw["content"]
    assert content["file"] == game_id.removeprefix("atari-").replace("-", "_") + ".bin"
    assert len(content["md5"]) == 32, "ALE's game table identifies the ROM by MD5"
