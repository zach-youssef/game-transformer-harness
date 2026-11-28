"""Human reference scores (docs/evaluation.md, "Human reference"): `gth reference-scores`
scores test-core credits recorded from the game's reset, frame-exact at the first frame the
game's episode.terminate holds (or the protocol's cap), and refuses every recording that isn't
one whole credit: another recorder, a rewind, a clip, another start, an unfinished credit."""

import dataclasses
import json
import statistics
from pathlib import Path

import pytest

from gtharness import _native
from gtharness.cli import main
from gtharness.env import GAMES_DIR
from gtharness.errors import ConfigError
from gtharness.protocol import loads
from gtharness.reference import MIN_CREDITS, reference_scores

A = 1 << 8
CAP = 200

# The agent's settings (frameskip, sticky actions, no-ops, observations, the action set) don't
# apply to a person; the cap and the score do.
PROTOCOL = f"""
backend = "libretro"
games = ["testcore"]
frameskip = 4
sticky_prob = 0.25
maxpool = false
frame_stack = 1
action_set = "atari_minimal"
max_episode_frames = {CAP}
life_loss_terminal = false
reward_clip = "none"
score_var = "score"
episodes_per_seed = 1
env_seeds = [0]
policy_seeds = [0]

[obs]
size = [12, 10]
crop = "game"
orientation = "game"
interpolation = "area"
grayscale = false

[start]
reset = "game"
noop_max = 5
"""


@pytest.fixture
def content(testcore_data: Path) -> Path:
    return testcore_data.parent / "roms" / "sub" / "renamed.gthc"


@pytest.fixture
def config() -> _native.GameConfig:
    return _native.GameConfig.load(GAMES_DIR / "testcore" / "game.toml")


@pytest.fixture
def credit(testcore_path, testcore_data, content, config, tmp_path):
    """``credit(masks, before=0, recorder="human")``: a recording of one frame per mask from
    the game's reset (after ``before`` more idle frames), and the score where the game's
    episode first terminates in it (None if it never does) minus at its start, read from the
    core's RAM as it plays."""

    made = iter(range(1000))

    def record(masks, *, before=0, recorder="human"):
        directory = tmp_path / "recordings" / f"credit-{next(made)}.gthrec"
        directory.parent.mkdir(exist_ok=True)
        core = _native.Core(
            testcore_path,
            content,
            testcore_data / "system",
            config.core_options,
            config.settle_frames,
            8,
            8,
            None,
            0,
            "area",
            False,
            False,
        )
        try:
            core.reset()
            core.run_inputs(config.reset_inputs)
            if before:
                core.run_inputs([(0, before)])
            core.start_recording(
                directory, config, "testcore", None, recorder, "tester", "CC0-1.0", 7
            )
            start = core.read_ram(config)[0]["score"]
            score = None
            for mask in masks:
                core.run_inputs([(mask, 1)])
                ram = core.read_ram(config)[0]
                if score is None and config.terminated(ram):
                    score = ram["score"] - start
            path = core.finish_recording()
        finally:
            core.close()
        assert path is not None
        return path, score

    return record


def score(testcore_path, testcore_data, recordings, games_dir=GAMES_DIR, **changes):
    protocol = dataclasses.replace(loads(PROTOCOL), **changes)
    return reference_scores(
        protocol,
        recordings,
        testcore_data,
        rom_dir=None,
        core_path=testcore_path,
        games_dir=games_dir,
    )


def test_credits_score_where_the_episode_first_terminates(testcore_path, testcore_data, credit):
    # Play goes on past game over, which doesn't count; the scores differ by when A is held.
    patterns = [
        [A] * 60,
        [A, 0] * 40,
        [0] * 15 + [A] * 30 + [0] * 10,
    ]
    made = [credit(masks) for masks in patterns]
    assert all(expected is not None for _, expected in made)
    result = score(testcore_path, testcore_data, [path for path, _ in made])

    game = result["games"]["testcore"]
    by_name = {path.name: expected for path, expected in made}
    assert game["recordings"] == sorted(by_name)
    assert [c["recording"] for c in game["credits"]] == game["recordings"]
    for entry in game["credits"]:
        assert entry["score"] == by_name[entry["recording"]]
        assert entry["terminated"] and not entry["truncated"]
        assert entry["player"] == "tester"
        assert 0 < entry["frames"] < 80
    scores = [entry["score"] for entry in game["credits"]]
    assert game["n"] == 3
    assert game["mean"] == statistics.fmean(scores)
    assert game["median"] == statistics.median(scores)
    assert f"fewer than {MIN_CREDITS}" in game["warning"]
    assert result["protocol"]["human"] == {
        "start_reset": "game",
        "max_episode_frames": CAP,
        "score_var": "score",
    }
    assert len(result["protocol"]["sha1"]) == 40

    # Enough credits: no warning.
    more = [credit(patterns[i % 3])[0] for i in range(MIN_CREDITS - 3)]
    result = score(testcore_path, testcore_data, [path for path, _ in made] + more)
    assert result["games"]["testcore"]["n"] == MIN_CREDITS
    assert "warning" not in result["games"]["testcore"]


def test_a_credit_past_the_cap_is_truncated_there(testcore_path, testcore_data, credit):
    path, _ = credit([A, 0, 0, 0] * 30)
    entry = score(testcore_path, testcore_data, [path], max_episode_frames=15)["games"]["testcore"][
        "credits"
    ][0]
    assert entry["frames"] == 15
    assert not entry["terminated"] and entry["truncated"]
    assert entry["score"] > 0


def test_a_game_without_credits_has_none(testcore_path, testcore_data):
    game = score(testcore_path, testcore_data, [])["games"]["testcore"]
    assert (game["n"], game["mean"], game["median"], game["credits"]) == (0, None, None, [])
    assert "warning" in game


def _edit_meta(path: Path, extra: str) -> None:
    meta = path / "meta.toml"
    meta.write_text(meta.read_text(encoding="utf-8") + extra, encoding="utf-8")


@pytest.mark.parametrize(
    ("make", "error"),
    [
        (lambda credit: credit([A] * 60, recorder="scripted")[0], "recorder 'scripted'"),
        (lambda credit: credit([A] * 60, before=3)[0], "doesn't start at the game's reset"),
        (lambda credit: credit([A] * 10)[0], "not a whole credit"),
    ],
    ids=["recorder", "start", "unfinished"],
)
def test_what_isnt_one_credit_is_refused(testcore_path, testcore_data, credit, make, error):
    good, _ = credit([A] * 60)
    with pytest.raises(ConfigError, match=error):
        score(testcore_path, testcore_data, [good, make(credit)])


@pytest.mark.parametrize(
    ("extra", "error"),
    [
        ('\n[[rewind]]\nframe = 5\ncause = "manual"\nclip = "x.gthrec"\n', r"\[\[rewind\]\]"),
        ('\n[branch]\nparent = "x.gthrec"\nframe = 5\ncause = "lives"\n', r"\[branch\]"),
    ],
    ids=["rewind", "branch"],
)
def test_rewinds_and_clips_are_refused(testcore_path, testcore_data, credit, extra, error):
    path, _ = credit([A] * 60)
    _edit_meta(path, extra)
    with pytest.raises(ConfigError, match=error):
        score(testcore_path, testcore_data, [path])


def test_protocols_that_dont_fit_a_person_are_refused(
    testcore_path, testcore_data, credit, tmp_path
):
    path, _ = credit([A] * 60)
    with pytest.raises(ConfigError, match="life_loss_terminal"):
        score(testcore_path, testcore_data, [path], life_loss_terminal=True)
    # A protocol of another game, whose files are all there.
    other = tmp_path / "games" / "testcore-copy"
    other.mkdir(parents=True)
    other.joinpath("game.toml").write_bytes((GAMES_DIR / "testcore" / "game.toml").read_bytes())
    with pytest.raises(ConfigError, match="isn't one of the protocol's"):
        score(testcore_path, testcore_data, [path], other.parent, games=("testcore-copy",))
    with pytest.raises(ConfigError, match="given twice"):
        score(testcore_path, testcore_data, [path, path])


def test_gth_reference_scores_writes_the_json(
    testcore_path, testcore_data, credit, tmp_path, capsys
):
    protocol = tmp_path / "protocol.toml"
    protocol.write_text(PROTOCOL, encoding="utf-8")
    path, expected = credit([A] * 60)
    out = tmp_path / "ref.json"
    common = [str(testcore_data), "--protocol", str(protocol), "--out", str(out)]
    common += ["--core-path", str(testcore_path)]
    assert main(["reference-scores", *common, str(path)]) == 0
    result = json.loads(out.read_text(encoding="utf-8"))
    assert result["games"]["testcore"]["credits"][0]["score"] == expected
    assert "testcore: 1 credits" in capsys.readouterr().out

    _edit_meta(path, '\n[branch]\nparent = "x.gthrec"\nframe = 5\ncause = "lives"\n')
    out.unlink()
    assert main(["reference-scores", *common, str(path)]) == 2
    assert "[branch]" in capsys.readouterr().err
    assert not out.exists()


def test_games_without_credits_need_no_files(testcore_path, testcore_data, credit):
    # Gunhed's ROM isn't there: only the games with credits are checked and loaded.
    path, expected = credit([A] * 60)
    games = score(testcore_path, testcore_data, [path], games=("testcore", "gunhed"))["games"]
    assert list(games) == ["testcore", "gunhed"]
    assert games["testcore"]["credits"][0]["score"] == expected
    assert (games["gunhed"]["n"], games["gunhed"]["mean"], games["gunhed"]["credits"]) == (
        0,
        None,
        [],
    )


def test_a_meta_without_its_fields_is_refused(testcore_path, testcore_data, credit):
    path, _ = credit([A] * 60)
    meta = path / "meta.toml"
    lines = meta.read_text(encoding="utf-8").splitlines()
    meta.write_text(
        "\n".join(line for line in lines if not line.startswith("player")) + "\n",
        encoding="utf-8",
    )
    with pytest.raises(ConfigError, match="no player"):
        score(testcore_path, testcore_data, [path])
