"""Scripted captures (docs/recording-format.md, "Scripted captures"): `capture` and
``gth capture`` on the test core record the script's inputs as a ``scripted`` recording that
replays, the same seed giving the same inputs."""

import tomllib
from pathlib import Path

import numpy as np
import pytest

from gtharness import capture as capture_module
from gtharness._native import GameConfig
from gtharness.capture import GAP, PULSE, SELECT, START, capture, player_name, script_steps
from gtharness.cli import main
from gtharness.env import GAMES_DIR, find_content
from gtharness.errors import ConfigError
from gtharness.record import Replayer

FRAMESKIP = 2
FRAMES = 3000  # more than two credit cycles of random-play


def run(testcore_path: Path, testcore_data: Path, out: Path, script: str, seed, **kwargs) -> Path:
    fields = {"frames": FRAMES, "frameskip": FRAMESKIP, "games_dir": GAMES_DIR} | kwargs
    return capture(
        "testcore",
        testcore_data,
        out,
        script=script,
        seed=seed,
        license="CC0-1.0",
        checks_every=60,
        core_path=testcore_path,
        **fields,
    )


def replay_masks(path: Path, testcore_path: Path) -> list[tuple[int, int]]:
    """Each recorded step's mask and frames, replayed to the end (a desync raises)."""
    config = GameConfig.load(GAMES_DIR / "testcore" / "game.toml")
    content = find_content(config, "testcore", None)
    replay = Replayer(path, testcore_path, content, content.parent, 8, 8, None, 0, "area")
    steps = []
    try:
        while (step := replay.step()) is not None:
            steps.append(step[:2])
    finally:
        replay.close()
    return steps


def meta(path: Path) -> dict:
    return tomllib.loads((path / "meta.toml").read_text(encoding="utf-8"))


def test_attract_holds_nothing_and_replays(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    path = run(testcore_path, testcore_data, tmp_path, "attract", None)
    fields = meta(path)
    assert fields["recorder"] == "scripted" and fields["player"] == "attract-v1"
    assert fields["license"] == "CC0-1.0" and fields["checks_every"] == 60
    assert (fields["frames"], fields["steps"]) == (FRAMES, FRAMES // FRAMESKIP)
    assert replay_masks(path, testcore_path) == [(0, FRAMESKIP)] * (FRAMES // FRAMESKIP)


def test_random_play_is_seeded_credits_and_plays_mapped_slots(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    first = run(testcore_path, testcore_data, tmp_path / "a", "random-play", 7)
    again = run(testcore_path, testcore_data, tmp_path / "b", "random-play", 7)
    other = run(testcore_path, testcore_data, tmp_path / "c", "random-play", 8)
    assert meta(first)["player"] == "random-play-v1 seed=7"
    steps = replay_masks(first, testcore_path)
    assert replay_masks(again, testcore_path) == steps
    assert replay_masks(other, testcore_path) != steps
    assert len(steps) == FRAMES // FRAMESKIP and {frames for _, frames in steps} == {FRAMESKIP}
    masks = np.array([mask for mask, _ in steps])
    # The reset's first observation, then a credit cycle every CREDIT_EVERY frames.
    pulse, gap = PULSE // FRAMESKIP, GAP // FRAMESKIP
    every = capture_module.CREDIT_EVERY // FRAMESKIP
    for start in range(1, len(masks), every):
        cycle = [SELECT] * pulse + [0] * gap + [START] * pulse + [0] * gap
        assert list(masks[start : start + len(cycle)]) == cycle[: len(masks) - start]
    # Everything else is the game's actions, each held at least HOLD[0] frames.
    config = GameConfig.load(GAMES_DIR / "testcore" / "game.toml")
    actions = set(config.joypad_table())
    played = masks[(masks & (SELECT | START)) == 0]
    assert set(played.tolist()) <= actions
    assert len(set(played.tolist())) > 20  # directions and slots vary
    shot = config.joypad_table()[config.encode(0, 1)]
    assert (played & shot).any() and ((played & shot) == 0).any()


def test_script_arguments_are_checked(tmp_path: Path):
    config = GameConfig.load(GAMES_DIR / "testcore" / "game.toml")
    with pytest.raises(ConfigError, match="no seed"):
        script_steps(config, "attract", 10, 1, 3)
    with pytest.raises(ConfigError, match="needs a seed"):
        script_steps(config, "random-play", 10, 1, None)
    with pytest.raises(ConfigError, match="script must be"):
        script_steps(config, "dance", 10, 1, None)
    text = (GAMES_DIR / "testcore" / "game.toml").read_text(encoding="utf-8")
    unarmed = GameConfig.parse(text.replace('"shot"', '"aux2"'), "no shot")
    with pytest.raises(ConfigError, match="shot slot"):
        script_steps(unarmed, "random-play", 10, 1, 3)
    assert player_name("random-play", 0) == "random-play-v1 seed=0"


def test_gth_capture(
    testcore_path: Path,
    testcore_data: Path,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
):
    out = tmp_path / "rec"
    args = [
        "capture",
        str(testcore_data),
        "testcore",
        "--out",
        str(out),
        "--script",
        "random-play",
        "--frames",
        "600",
        "--frameskip",
        "4",
        "--seed",
        "1",
        "--license",
        "CC0-1.0",
        "--checks-every",
        "60",
        "--core-path",
        str(testcore_path),
    ]
    assert main(args) == 0
    output = capsys.readouterr()
    (path,) = out.iterdir()
    assert output.out.strip() == str(path)
    assert "testcore: 600/600 frames" in output.err
    assert (meta(path)["frames"], meta(path)["steps"]) == (600, 150)
    for option, value, message in [
        ("--frames", "601", "multiple of frameskip"),
        ("--frameskip", "0", "frameskip must be"),
        ("--checks-every", "0", "checks_every"),
    ]:
        bad = args.copy()
        bad[bad.index(option) + 1] = value
        assert main(bad) == 2
        assert message in capsys.readouterr().err
    no_seed = args[: args.index("--seed")] + args[args.index("--seed") + 2 :]
    assert main(no_seed) == 2
    assert "needs a seed" in capsys.readouterr().err
    bad_core = args.copy()
    bad_core[bad_core.index("--core-path") + 1] = str(tmp_path / "missing.dylib")
    assert main(bad_core) == 2  # CoreLoadError, like ConfigError, is a HarnessError
    assert capsys.readouterr().err.startswith("gth capture: ")
    assert len(list(out.iterdir())) == 1  # no failed capture left a recording
    with pytest.raises(SystemExit):
        main(args[: args.index("--license")])  # The license is required.
