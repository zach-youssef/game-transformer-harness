"""The Atari games on the ALE core (docs/games.md, "Atari (ALE core)"): every config follows
the shared recipe, and the Atari presets name exactly those games. With the ROMs ale-py
bundles (docs/roms.md, "Atari ROMs") and the pinned core, the games also play: episodes are
deterministic, a sticky-action recording replays, the score is ALE's reward sum, a lost life
isn't terminal, the 108k-frame cap truncates and the gray palette is ALE's. The ROM tests skip
without the ROMs or the core, as on CI (docs/testing.md); scripts/ale_parity.py compares the
core with ale-py itself."""

import hashlib
import tomllib
from collections.abc import Callable
from pathlib import Path

import numpy as np
import pytest

from gtharness import _native
from gtharness.env import GAMES_DIR, RetroEnv
from gtharness.protocol import load
from gtharness.record import Record, Replayer
from gtharness.rewards import score_delta

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


def test_every_atari_game_has_cited_human_and_random_scores():
    path = Path(__file__).parent.parent / "protocols" / "references" / "atari-human-random.toml"
    table = tomllib.loads(path.read_text(encoding="utf-8"))
    assert sorted(table) == ATARI
    for game, tables in table.items():
        assert "badia2020agent57" in tables, f"{game}: Agent57's table covers every game"
        for name, row in tables.items():
            assert set(row) == {"random", "human", "claim"}, (game, name)
            assert row["claim"].startswith(f"{name}#C"), (game, name)
            assert row["human"] > row["random"], (game, name)


ALE_FRAME = (210, 160)
NOOP, FIRE = 0, 1  # flat actions: nothing; no direction with the shot slot (b)
# ALE v5's frame cap (max_num_frames_per_episode), the Atari presets' max_episode_frames.
ALE_V5_CAP = 108_000


def make(
    game_id: str,
    core_path: Path,
    *,
    frameskip: int = 4,
    obs_size: tuple[int, int] = (84, 84),
    sticky_prob: float = 0.0,
    max_episode_frames: int | None = None,
    games_dir: Path = GAMES_DIR,
    record: Record | None = None,
) -> RetroEnv:
    return RetroEnv.from_game(
        game_id,
        core_path.parent.parent,
        frameskip=frameskip,
        obs_size=obs_size,
        action_format="flat",
        reward=score_delta,
        sticky_prob=sticky_prob,
        max_episode_frames=max_episode_frames,
        noop_max=0,
        maxpool=False,
        grayscale=False,
        core_path=core_path,
        games_dir=games_dir,
        record=record,
    )


def with_config(tmp_path: Path, game_id: str, edit: Callable[[str], str]) -> Path:
    """A games directory holding ``game_id``'s config with ``edit`` applied."""
    toml = (GAMES_DIR / game_id / "game.toml").read_text(encoding="utf-8")
    changed = edit(toml)
    assert changed != toml
    (tmp_path / "games" / game_id).mkdir(parents=True)
    (tmp_path / "games" / game_id / "game.toml").write_text(changed, encoding="utf-8")
    return tmp_path / "games"


def play(env: RetroEnv, actions: list[int], seed: int) -> list[tuple[str, dict, bool, bool]]:
    """Each step's frame hash, RAM and end flags, from a reset with ``seed``."""
    obs, info = env.reset(seed=seed)
    steps = [(hashlib.sha1(obs.tobytes()).hexdigest(), info["ram"], False, False)]
    for action in actions:
        obs, _, terminated, truncated, info = env.step(action)
        steps.append((hashlib.sha1(obs.tobytes()).hexdigest(), info["ram"], terminated, truncated))
        if terminated or truncated:
            break
    return steps


def test_episodes_are_deterministic(rom, core):
    rom("atari-ms-pacman")
    core_path = core("atari-ms-pacman")
    actions = np.random.default_rng(0).integers(0, 9 * 16, 600).tolist()
    # One core per process, so the second env loads the core again after the first closes.
    env = make("atari-ms-pacman", core_path, sticky_prob=0.25)
    try:
        played = play(env, actions, seed=5)
        assert len({frame for frame, *_ in played}) > 100, "the game moves"
        assert played[-1][1]["score"] > 0
        assert play(env, actions, seed=5) == played, "the same seed and actions, again"
        assert play(env, actions, seed=6) != played, "the seed draws the sticky actions"
    finally:
        env.close()
    env = make("atari-ms-pacman", core_path, sticky_prob=0.25)
    try:
        assert play(env, actions, seed=5) == played, "and in a fresh load"
    finally:
        env.close()


def test_a_sticky_action_recording_replays_without_desync(rom, core, tmp_path: Path):
    content = rom("atari-space-invaders")
    core_path = core("atari-space-invaders")
    record = Record(
        directory=tmp_path, recorder="scripted", player="random", license="CC0-1.0", checks_every=30
    )
    env = make("atari-space-invaders", core_path, sticky_prob=0.25, record=record)
    rng = np.random.default_rng(1)
    try:
        env.reset(seed=3)
        steps = 0
        terminated = False
        while not terminated:
            _, _, terminated, _, info = env.step(int(rng.integers(0, 9 * 16)))
            steps += 1
        assert info["ram"]["game_over"] == 1 and info["ram"]["score"] > 0
    finally:
        env.close()
    (path,) = env.recordings
    game = _native.GameConfig.load(GAMES_DIR / "atari-space-invaders" / "game.toml")
    replay = Replayer(path, core_path, content, core_path.parent.parent / "system", 84, 84,
                      game.crop, game.orientation, "area")  # fmt: skip
    try:
        # A desync check fails with DesyncError at its frame; there is none.
        assert replay.steps == steps + 1, "the reset, then the steps"
        assert replay.run_to_end() == replay.frames
    finally:
        replay.close()


def test_the_score_is_the_sum_of_ales_rewards(rom, core, tmp_path: Path):
    """Pong's reward is the change of (player's points - computer's points), RAM 14 and 13
    (ALE's Pong.cpp): read here from the game's own RAM, next to the core's score."""
    rom("atari-pong")
    core_path = core("atari-pong")
    points = """
[ram.cpu_points]
memory = "system_ram"
offset = 13
size = 1
encoding = "le"

[ram.player_points]
memory = "system_ram"
offset = 14
size = 1
encoding = "le"

[episode]"""
    games = with_config(tmp_path, "atari-pong", lambda toml: toml.replace("\n[episode]", points))
    env = make("atari-pong", core_path, games_dir=games)
    rng = np.random.default_rng(2)
    try:
        _, info = env.reset(seed=0)
        total, seen, terminated = 0.0, set(), False
        while not terminated:
            # The paddle's moves and fire, so the player wins some points.
            _, reward, terminated, truncated, info = env.step(int(rng.choice([0, 1, 48, 112])))
            ram = info["ram"]
            total += reward
            assert ram["score"] == ram["player_points"] - ram["cpu_points"] == total
            seen.add(reward)
            assert not truncated
        assert ram["game_over"] == 1 and 21 in (ram["cpu_points"], ram["player_points"])
        assert {-1.0, 0.0} <= seen, "rewards are each point's"
    finally:
        env.close()


def test_a_lost_life_is_not_terminal(rom, core):
    rom("atari-space-invaders")
    core_path = core("atari-space-invaders")
    env = make("atari-space-invaders", core_path)
    try:
        _, info = env.reset(seed=0)
        lives = [info["ram"]["lives"]]
        terminated = False
        while not terminated:
            _, _, terminated, truncated, info = env.step(NOOP)
            assert not truncated
            if info["ram"]["lives"] != lives[-1]:
                assert not terminated or info["ram"]["game_over"], "a life lost isn't the end"
                lives.append(info["ram"]["lives"])
        assert lives == [3, 2, 1, 0] and info["ram"]["game_over"] == 1
    finally:
        env.close()


def test_the_ale_v5_cap_truncates(rom, core):
    """Seaquest never ends without input (its submarine waits at the surface)."""
    rom("atari-seaquest")
    core_path = core("atari-seaquest")
    env = make("atari-seaquest", core_path, frameskip=8, obs_size=(8, 8),
               max_episode_frames=ALE_V5_CAP)  # fmt: skip
    try:
        _, info = env.reset(seed=0)
        steps = 0
        truncated = False
        while not truncated:
            _, _, terminated, truncated, info = env.step(NOOP)
            assert not terminated
            steps += 1
        assert info["episode_frame"] == ALE_V5_CAP and steps == (ALE_V5_CAP - 8) // 8
        assert info["ram"]["game_over"] == 0 and info["ram"]["lives"] == 4
    finally:
        env.close()


def test_the_gray_palette_is_ales(rom, core, tmp_path: Path):
    """``ale_gray = "on"`` draws each pixel as round(0.2989 r + 0.587 g + 0.114 b) of ALE's
    colour palette (ALE's ColourPalette.cpp), r = g = b: checked on Ms. Pac-Man's colours."""
    rom("atari-ms-pacman")
    core_path = core("atari-ms-pacman")
    games = with_config(tmp_path, "atari-ms-pacman", lambda toml: toml + 'ale_gray = "on"\n')
    actions = np.random.default_rng(3).integers(0, 9 * 16, 400).tolist()
    frames = {}
    for name, directory in (("colour", GAMES_DIR), ("gray", games)):
        env = make("atari-ms-pacman", core_path, frameskip=1, obs_size=ALE_FRAME,
                   games_dir=directory)  # fmt: skip
        try:
            frames[name] = [env.reset(seed=0)[0]] + [env.step(a)[0] for a in actions]
        finally:
            env.close()
    colours = set()
    for rgb, grey in zip(frames["colour"], frames["gray"], strict=True):
        assert (grey == grey[..., :1]).all(), "r = g = b"
        lum = rgb.astype(np.float64) @ np.array([0.2989, 0.5870, 0.1140])
        # C's round(): halves away from zero.
        assert (grey[..., 0] == np.floor(lum + 0.5).astype(np.uint8)).all()
        colours |= set(map(tuple, rgb.reshape(-1, 3).tolist()))
    assert len(colours) >= 8, "enough of the palette to mean something"
