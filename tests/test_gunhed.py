"""games/gunhed on the real core and the user's ROM: a scripted session from the reset that
scores, loses every ship and ends at GAME OVER, then replays its recording (docs/games.md,
"Adding a game"). Skips without the ROM or the pinned core, as on CI (docs/testing.md)."""

import tomllib
from pathlib import Path

import pytest

from gtharness import _native
from gtharness.env import GAMES_DIR, RetroEnv
from gtharness.ramsearch import assert_ram, run_script
from gtharness.record import Record, Replayer
from gtharness.rewards import score_delta

FRAMESKIP = 4
OBS = (84, 84)
SHOT = 1  # flat action: no direction, the shot slot (game.toml: b)
UP = 1 * 16  # flat action: up, no button (docs/actions.md)
SHOOTING_STEPS = 150
SEARCHES = GAMES_DIR / "gunhed" / "searches"


def test_a_scripted_session_scores_dies_ends_and_replays(rom, core, tmp_path: Path):
    content = rom("gunhed")
    core_path = core("gunhed")
    env = RetroEnv.from_game(
        "gunhed",
        core_path.parent.parent,
        frameskip=FRAMESKIP,
        obs_size=OBS,
        action_format="flat",
        reward=score_delta,
        core_path=core_path,
        record=Record(
            directory=tmp_path,
            recorder="scripted",
            player="scripted",
            license="CC0-1.0",
            checks_every=60,
        ),
    )
    try:
        _, info = env.reset()
        assert info["ram"] == {"score": 0, "lives": 2, "bombs": 3, "stage": 1, "mode": 0}

        # Shoot the first wave from where the ship starts.
        total = 0.0
        for _ in range(SHOOTING_STEPS):
            _, reward, terminated, _, info = env.step(SHOT)
            total += reward
            assert not terminated
        assert_ram(info["ram"], "score", 1300)
        assert total == 1300, "score_delta pays the score's rise"

        # Then fly up into the enemies without shooting: each hit costs a ship, and the third
        # ends the game.
        deaths = []
        steps = SHOOTING_STEPS
        terminated = truncated = False
        while not (terminated or truncated):
            lives = info["ram"]["lives"]
            _, reward, terminated, truncated, info = env.step(UP)
            total += reward
            steps += 1
            if info["ram"]["lives"] < lives:
                deaths.append((steps, info["ram"]["lives"]))
            assert steps < 2000, "no GAME OVER"
        assert deaths == [(164, 1), (419, 0)]
        assert terminated and not truncated
        assert (steps, info["episode_frame"]) == (692, 3256)
        assert_ram(info["ram"], "mode", 0x40)
        assert_ram(info["ram"], "lives", 0)
        assert_ram(info["ram"], "score", 1300)
        assert total == 1300
    finally:
        env.close()

    (path,) = env.recordings
    game = _native.GameConfig.load(GAMES_DIR / "gunhed" / "game.toml")
    replay = Replayer(
        path,
        core_path,
        content,
        core_path.parent.parent / "system",
        *OBS,
        game.crop,
        game.orientation,
        "area",
    )
    try:
        # A desync check fails with DesyncError at its frame; there is none.
        assert (replay.frames, replay.steps) == (693 * FRAMESKIP, 693), "the reset, then 692"
        assert replay.run_to_end() == replay.frames
    finally:
        replay.close()


def test_an_area_clear_does_not_end_the_episode(rom, core):
    """0x375 is 0x80 while an area is cleared and 0x40 at GAME OVER: only the second ends the
    episode. From a savestate ~600 frames before Area 1's clear, sitting still, the clear
    plays out into Area 2."""
    rom("gunhed")
    core_path = core("gunhed")
    state = SEARCHES / "states" / "area1-end.state"
    if not state.is_file():
        pytest.skip(f"gunhed: {state.name} isn't there (games/gunhed/README.md)")
    env = RetroEnv.from_game(
        "gunhed",
        core_path.parent.parent,
        frameskip=FRAMESKIP,
        obs_size=OBS,
        action_format="flat",
        reward=score_delta,
        core_path=core_path,
    )
    try:
        _, info = env.reset(options={"state": state.read_bytes()})
        assert_ram(info["ram"], "stage", 1)
        changes = []
        previous = (info["ram"]["mode"], info["ram"]["stage"])
        for step in range(1, 321):
            _, _, terminated, truncated, info = env.step(0)
            assert not (terminated or truncated), f"ended at step {step}: {info['ram']}"
            now = (info["ram"]["mode"], info["ram"]["stage"])
            if now != previous:
                changes.append((step, *now))
            previous = now
    finally:
        env.close()
    # (step, mode, stage): the clear starts, the area number rises, Area 2 plays.
    assert changes == [(159, 0x80, 1), (272, 0x80, 2), (295, 0, 2)]


@pytest.mark.parametrize("name", ["score", "lives", "bombs", "mode", "stage"])
def test_search_script_finds_the_configs_address(rom, core, name: str):
    """Each script in games/gunhed/searches/ narrows to its game.toml variable alone: the
    evidence for the addresses, rerun so a core bump that moves them is caught."""
    rom("gunhed")
    core_path = core("gunhed")
    script = SEARCHES / f"{name}.toml"
    states = [
        SEARCHES / step["state"]
        for step in tomllib.loads(script.read_text())["step"]
        if "state" in step
    ]
    missing = [state.name for state in states if not state.is_file()]
    if missing:
        pytest.skip(
            f"gunhed: {name}.toml's savestates {missing} aren't there (games/gunhed/README.md)"
        )
    env = RetroEnv.from_game(
        "gunhed",
        core_path.parent.parent,
        frameskip=1,
        obs_size=OBS,
        action_format="flat",
        reward=score_delta,
        core_path=core_path,
    )
    try:
        env.reset()
        search, _ = run_script(env, script)
        found = search.candidates(10)
    finally:
        env.close()
    variable = tomllib.loads((GAMES_DIR / "gunhed" / "game.toml").read_text())["ram"][name]
    assert {(c["memory"], c["offset"]) for c in found} == {(variable["memory"], variable["offset"])}
    assert (variable["size"], variable["encoding"]) in {(c["size"], c["encoding"]) for c in found}
