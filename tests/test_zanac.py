"""games/zanac on the real core and the user's ROM: a scripted session from the reset that
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
LEFT = 7 * 16  # flat action: left, no button (docs/actions.md)
SHOOTING_STEPS = 150  # the score reads 230 after them
SEARCHES = GAMES_DIR / "zanac" / "searches"


def test_a_scripted_session_scores_dies_ends_and_replays(rom, core, tmp_path: Path):
    content = rom("zanac")
    core_path = core("zanac")
    env = RetroEnv.from_game(
        "zanac",
        core_path.parent.parent,
        frameskip=FRAMESKIP,
        obs_size=OBS,
        action_format="flat",
        reward=score_delta,
        sticky_prob=0.0,
        max_episode_frames=None,
        noop_max=0,
        maxpool=False,
        grayscale=False,
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
        assert info["ram"] == {"score": 0, "lives": 3, "stage": 1, "playing": 0}

        # Shoot from where the ship starts: the digits of the score read as the HUD's number.
        total = 0.0
        for _ in range(SHOOTING_STEPS):
            _, reward, terminated, _, info = env.step(SHOT)
            total += reward
            assert not terminated
        assert_ram(info["ram"], "score", 230)
        assert total == 230, "score_delta pays the score's rise"
        assert_ram(info["ram"], "lives", 2)  # the first ship went at step 53

        # Keep shooting: each hit costs a ship, and the third (the first was lost above) ends the
        # game.
        deaths = []
        steps = SHOOTING_STEPS
        terminated = truncated = False
        while not (terminated or truncated):
            lives = info["ram"]["lives"]
            _, reward, terminated, truncated, info = env.step(SHOT)
            total += reward
            steps += 1
            if info["ram"]["lives"] < lives:
                deaths.append((steps, info["ram"]["lives"]))
            assert steps < 2000, "no GAME OVER"
        assert deaths == [(215, 1), (283, 0)]
        assert terminated and not truncated
        assert (steps, info["episode_frame"]) == (283, 1262)
        assert_ram(info["ram"], "lives", 0)
        assert_ram(info["ram"], "stage", 1)
        assert_ram(info["ram"], "score", 1470)
        assert total == 1470
    finally:
        env.close()

    (path,) = env.recordings
    game = _native.GameConfig.load(GAMES_DIR / "zanac" / "game.toml")
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
        assert (replay.frames, replay.steps) == (284 * FRAMESKIP, 284), "the reset, then 283"
        assert replay.run_to_end() == replay.frames
    finally:
        replay.close()


def ship_x(env: RetroEnv) -> int:
    """The ship's x from the sprite copy at $0200 (4 bytes each: y, tile, attributes, x): its
    left half is the sprite with a ship tile (0x14 to 0x26) at y 152, the row it flies on."""
    oam = env.memory(2)[0x200:0x300]
    xs = [oam[i + 3] for i in range(0, 256, 4) if oam[i] == 152 and oam[i + 1] in range(0x14, 0x28)]
    assert xs, "no ship sprite"
    return min(xs)


def test_the_ship_answers_the_d_pad_from_the_first_action(rom, core):
    """The reset script ends where the ship first answers the d-pad: after one step holding
    left the ship is left of where it stands without input (118 against 128 at frameskip 4)."""
    rom("zanac")
    core_path = core("zanac")
    positions = {}
    for name, action in {"still": 0, "left": LEFT}.items():
        env = RetroEnv.from_game(
            "zanac",
            core_path.parent.parent,
            frameskip=FRAMESKIP,
            obs_size=OBS,
            action_format="flat",
            reward=score_delta,
            sticky_prob=0.0,
            max_episode_frames=None,
            noop_max=0,
            maxpool=False,
            grayscale=False,
            core_path=core_path,
        )
        try:
            env.reset()
            env.step(action)
            positions[name] = ship_x(env)
        finally:
            env.close()
    assert positions["left"] < positions["still"], positions


def test_an_area_clear_does_not_end_the_episode(rom, core):
    """`playing` drops while an area is cleared, `stage` rises and the episode goes on. From a
    savestate ~600 frames before Area 1's clear (with many ships, so the ship survives the
    wait), sitting still, the clear plays out into Area 2."""
    rom("zanac")
    core_path = core("zanac")
    state = SEARCHES / "states" / "area1-end.state"
    if not state.is_file():
        pytest.skip(f"zanac: {state.name} isn't there (games/zanac/README.md)")
    env = RetroEnv.from_game(
        "zanac",
        core_path.parent.parent,
        frameskip=FRAMESKIP,
        obs_size=OBS,
        action_format="flat",
        reward=score_delta,
        sticky_prob=0.0,
        max_episode_frames=None,
        noop_max=0,
        maxpool=False,
        grayscale=False,
        core_path=core_path,
    )
    try:
        _, info = env.reset(options={"state": state.read_bytes()})
        assert_ram(info["ram"], "stage", 1)
        changes = []
        previous = (info["ram"]["playing"], info["ram"]["stage"])
        for step in range(1, 301):
            _, _, terminated, _, info = env.step(0)
            assert not terminated
            now = (info["ram"]["playing"], info["ram"]["stage"])
            if now != previous:
                changes.append((step, *now))
            previous = now
    finally:
        env.close()
    # (step, playing, stage): the clear starts, the area number rises, Area 2 plays.
    assert changes == [(164, 0, 1), (241, 0, 2), (254, 1, 2)]


@pytest.mark.parametrize("name", ["lives", "stage", "playing"])
def test_search_script_finds_the_configs_address(rom, core, name: str):
    """Each script in games/zanac/searches/ narrows to its game.toml variable (`stage` also
    to 0x91 and `playing` to 0x31b, which hold the same values: nothing the scripts watch tells
    them apart), rerun so a core bump that moves them is caught. The score has none: it is
    digit bytes, which a search can't find (games/zanac/README.md)."""
    rom("zanac")
    core_path = core("zanac")
    script = SEARCHES / f"{name}.toml"
    states = [
        SEARCHES / step["state"]
        for step in tomllib.loads(script.read_text())["step"]
        if "state" in step
    ]
    missing = [state.name for state in states if not state.is_file()]
    if missing:
        pytest.skip(
            f"zanac: {name}.toml's savestates {missing} aren't there (games/zanac/README.md)"
        )
    env = RetroEnv.from_game(
        "zanac",
        core_path.parent.parent,
        frameskip=1,
        obs_size=OBS,
        action_format="flat",
        reward=score_delta,
        sticky_prob=0.0,
        max_episode_frames=None,
        noop_max=0,
        maxpool=False,
        grayscale=False,
        core_path=core_path,
    )
    try:
        env.reset()
        search, _ = run_script(env, script)
        found = search.candidates(10)
    finally:
        env.close()
    variable = tomllib.loads((GAMES_DIR / "zanac" / "game.toml").read_text())["ram"][name]
    offsets = {(c["memory"], c["offset"]) for c in found}
    copies = {"stage": {("system_ram", 0x91)}, "playing": {("system_ram", 0x31B)}}.get(name, set())
    assert offsets == {(variable["memory"], variable["offset"])} | copies
    assert (variable["size"], variable["encoding"]) in {(c["size"], c["encoding"]) for c in found}
