"""The evaluation runner (docs/evaluation.md, "Running an evaluation"): `gth evaluate` plays
the random policy on the test core under a test protocol, its results validate against the
documented schema and its recordings replay; a run the env or the policy can't honour is
refused before anything is written."""

import dataclasses
import json
import sys
import time
import tomllib
from pathlib import Path

import pytest

from gtharness import evaluate as evaluate_module
from gtharness._native import GameConfig
from gtharness.cli import main
from gtharness.env import GAMES_DIR, find_content
from gtharness.errors import ConfigError
from gtharness.evaluate import (
    EvaluationFailed,
    Recording,
    check_protocol,
    check_results,
    evaluate,
)
from gtharness.protocol import loads
from gtharness.record import Replayer

RANDOM = "gtharness.policies.random:RandomPolicy"

# Small enough to run in seconds: 2 policy seeds x 2 env seeds x 2 episodes. The test core
# scores while A (shot) is held and ends an episode at score >= 20 (games/testcore/game.toml),
# so random play ends some episodes and the cap truncates others.
PROTOCOL = """
backend = "libretro"
games = ["testcore"]
frameskip = 2
sticky_prob = 0.25
maxpool = false
frame_stack = 3
action_set = "harness"
max_episode_frames = 41
life_loss_terminal = false
reward_clip = "none"
score_var = "score"
episodes_per_seed = 2
env_seeds = [0, 7]
policy_seeds = [3, 4]

[obs]
size = [12, 10]
crop = "game"
orientation = "game"
interpolation = "area"
grayscale = false

[start]
reset = "game"
noop_max = 5
later_episodes = "reload"
"""


def protocol(**changes):
    """The test protocol with top-level, ``obs`` or ``start`` fields changed."""
    base = loads(PROTOCOL)
    obs = {k: v for k, v in changes.items() if k in {f.name for f in dataclasses.fields(base.obs)}}
    start = {k: v for k, v in changes.items() if k in ("reset", "noop_max", "later_episodes")}
    top = {k: v for k, v in changes.items() if k not in obs and k not in start}
    return dataclasses.replace(
        base,
        obs=dataclasses.replace(base.obs, **obs),
        start=dataclasses.replace(base.start, **start),
        **top,
    )


def run(testcore_path, testcore_data, out, policy=RANDOM, *, record=None, jobs=2, **changes):
    return evaluate(
        protocol(**changes),
        policy,
        out,
        testcore_data,
        policy_kwargs={},
        record=record,
        jobs=jobs,
        core_path=testcore_path,
    )


def rows(out: Path) -> list[dict]:
    return [json.loads(line) for line in (out / "episodes.jsonl").read_text().splitlines()]


def test_random_on_testcore_validates_and_its_recordings_replay(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    out = tmp_path / "results"
    status = main(
        [
            "evaluate",
            str(testcore_data),
            "--protocol",
            str(write_protocol(tmp_path)),
            "--policy",
            RANDOM,
            "--out",
            str(out),
            "--record",
            "--license",
            "CC0-1.0",
            "--checks-every",
            "8",
            "--jobs",
            "2",
            "--core-path",
            str(testcore_path),
        ]
    )
    assert status == 0
    results = check_results(out)
    run_info, episodes = results["run"], results["episodes"]
    assert results["protocol"] == loads(PROTOCOL)
    assert run_info["protocol_sha1"] == loads(PROTOCOL).expand().sha1
    assert run_info["policy"] == {
        "spec": RANDOM,
        "kwargs": {},
        "seeds": {seed: {"id": "random-v1", "checkpoint_sha256": None} for seed in ("3", "4")},
    }
    assert run_info["games"] == {
        "testcore": {
            "core": "gth_testcore",
            "content_sha1": "1a03ac5869e1d56cfd138db9790087dd926c18b3",
        }
    }
    assert run_info["harness"]["git_sha"] and run_info["recorded"] is True
    assert run_info["policy_calls"] == run_info["steps"]  # random: one action per call
    assert len(episodes) == 8
    assert {(row["policy_id"], row["checkpoint_sha256"]) for row in episodes} == {
        ("random-v1", None)
    }
    assert {row["terminated"] for row in episodes} == {True, False}, "both episode ends happen"
    assert all(row["truncated"] == (row["frames"] == 41) for row in episodes)

    config = GameConfig.load(GAMES_DIR / "testcore" / "game.toml")
    content = find_content(config, "testcore", None)
    for row in episodes:
        path = out / row["recording"]
        meta = tomllib.loads((path / "meta.toml").read_text(encoding="utf-8"))
        assert meta["recorder"] == "policy"
        assert meta["player"] == f"random-v1 checkpoint=none seed={row['policy_seed']}"
        assert meta["license"] == "CC0-1.0"
        replay = Replayer(path, testcore_path, content, content.parent, 8, 8, None, 0, "area")
        try:
            # The recording starts at the end of the reset script, as the episode's frames do;
            # a desync raises.
            assert replay.run_to_end() == row["frames"] == replay.frames
        finally:
            replay.close()


def write_protocol(directory: Path, text: str = PROTOCOL) -> Path:
    path = directory / "test-protocol.toml"
    path.write_text(text, encoding="utf-8")
    return path


def test_results_do_not_depend_on_jobs_and_seeds_matter(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    run(testcore_path, testcore_data, tmp_path / "a", jobs=1)
    run(testcore_path, testcore_data, tmp_path / "b", jobs=4)
    first, again = rows(tmp_path / "a"), rows(tmp_path / "b")
    assert first == again
    assert all(row["recording"] is None for row in first)
    check_results(tmp_path / "a")
    # Different seeds, different episodes; the RNGs run on within a pair.
    outcomes = [(row["score"], row["frames"], row["steps"]) for row in first]
    assert len(set(outcomes)) > 1
    # The policy's seed is derived from both seeds: one policy seed, two env seeds, two
    # streams of actions (the env seed alone changes only sticky actions and no-ops).
    run(testcore_path, testcore_data, tmp_path / "c", noop_max=0, sticky_prob=0.0)
    same_env = {}
    for row in rows(tmp_path / "c"):
        if row["episode"] == 0 and row["policy_seed"] == 3:
            same_env[row["env_seed"]] = (row["score"], row["steps"], row["frames"])
    assert same_env[0] != same_env[7]


def test_chained_episodes_carry_on_and_do_not_depend_on_jobs(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    """Under ``start.later_episodes = "chain"`` a pair's later episodes start from the core's
    soft reset where the last one ended: the test core keeps its score, so they end sooner
    than reloaded ones. Each pair still plays in one process, so ``--jobs`` changes nothing,
    and the recordings (each from its own start state) replay."""
    chain = {"later_episodes": "chain", "sticky_prob": 0.0, "noop_max": 0}
    run(testcore_path, testcore_data, tmp_path / "a", jobs=1, **chain)
    run(testcore_path, testcore_data, tmp_path / "b", jobs=4, **chain)
    run(testcore_path, testcore_data, tmp_path / "c", sticky_prob=0.0, noop_max=0)
    chained, reloaded = rows(tmp_path / "a"), rows(tmp_path / "c")
    assert chained == rows(tmp_path / "b")
    check_results(tmp_path / "a")
    assert 'later_episodes = "chain"' in (tmp_path / "a" / "protocol.toml").read_text()
    # Same seeds, same first episodes; the later ones start where the first ones ended.
    first = [row for row in chained if row["episode"] == 0]
    assert first == [row for row in reloaded if row["episode"] == 0]
    assert chained != reloaded

    out = tmp_path / "recorded"
    record = Recording(license="CC0-1.0", checks_every=8)
    run(testcore_path, testcore_data, out, record=record, **chain)
    assert [row | {"recording": None} for row in rows(out)] == chained
    config = GameConfig.load(GAMES_DIR / "testcore" / "game.toml")
    content = find_content(config, "testcore", None)
    for row in rows(out):
        replay = Replayer(
            out / row["recording"], testcore_path, content, content.parent, 8, 8, None, 0, "area"
        )
        try:
            assert replay.run_to_end() == row["frames"]
        finally:
            replay.close()


def test_frames_count_the_noops_and_steps_but_not_the_reset_script(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    run(testcore_path, testcore_data, tmp_path / "out", noop_max=0, sticky_prob=0.0)
    episodes = rows(tmp_path / "out")
    assert any(not row["truncated"] for row in episodes)
    for row in episodes:
        if not row["truncated"]:
            assert row["frames"] == 2 * (row["steps"] + 1), "the first observation, then steps"


CHUNK_POLICY = '''
import numpy as np
from gtharness.policies import ObsRequirements

class Chunks:
    """Plays chunks of three: shot+up, shot, nothing; checks the observations' shapes. Its
    checkpoint is the one of its policy seed, as a trained model's would be."""

    id = "chunks-v1"
    obs_requirements = ObsRequirements(
        size=(12, 10), grayscale=False, maxpool=None, frame_stack=3, frameskip=2,
        action_set="harness",
    )

    def __init__(self, *, policy_seed, chunk=3, checkpoints=None):
        self.chunk = chunk
        self.checkpoint_sha256 = (checkpoints or {}).get(str(policy_seed), "ab" * 32)

    def reset(self, seed, game):
        assert game.n_actions == 144 and game.game_id == "testcore"

    def act(self, obs):
        assert obs.shape == (3, 12, 10, 3) and obs.dtype == np.uint8
        return np.array([1 * 16 + 1, 1, 0][: self.chunk], dtype=np.int64)

class Gray(Chunks):
    """Chunks on max-pooled gray observations: one channel."""

    obs_requirements = ObsRequirements(
        size=(12, 10), grayscale=True, maxpool=True, frame_stack=3, frameskip=2,
        action_set="harness",
    )

    def act(self, obs):
        assert obs.shape == (3, 12, 10, 1) and obs.dtype == np.uint8
        return np.array([1 * 16 + 1, 1, 0][: self.chunk], dtype=np.int64)

class Atari(Chunks):
    """Chunks in ALE's action numbers (atari_full): UPFIRE, FIRE, NOOP are the flat actions
    Chunks plays."""

    obs_requirements = ObsRequirements(
        size=(12, 10), grayscale=False, maxpool=None, frame_stack=3, frameskip=2,
        action_set="atari_full",
    )

    def __init__(self, *, policy_seed, actions=(10, 1, 0)):
        super().__init__(policy_seed=policy_seed)
        self.actions = actions

    def reset(self, seed, game):
        assert game.n_actions == 18 and game.game_id == "testcore"
        assert game.atari_actions == tuple(range(18))
        assert game.mapped_slots == ("shot", "bomb", "aux1", "aux2")

    def act(self, obs):
        assert obs.shape == (3, 12, 10, 3) and obs.dtype == np.uint8
        return list(self.actions)

class Minimal(Atari):
    """Indices into the minimal set [NOOP, FIRE, UPFIRE] (ALE 0, 1, 10)."""

    obs_requirements = ObsRequirements(
        size=(12, 10), grayscale=False, maxpool=None, frame_stack=3, frameskip=2,
        action_set="atari_minimal",
    )

    def reset(self, seed, game):
        assert game.n_actions == 3 and game.game_id == "testcore"
        assert game.atari_actions == (0, 1, 10), "the ALE action behind each index"

class Wrong:
    def __init__(self, *, policy_seed):
        self.id = ""

class Missing:
    def __init__(self, *, policy_seed, checkpoint):
        open(checkpoint, "rb")

class Slow:
    """Hangs at policy seed 3, fails at 4; leaves a file per pair it starts."""

    id = "slow-v1"
    checkpoint_sha256 = None
    obs_requirements = Chunks.obs_requirements

    def __init__(self, *, policy_seed, started):
        self.seed, self.started = policy_seed, started

    def reset(self, seed, game):
        open(f"{self.started}/{self.seed}", "w").close()
        if self.seed == 4:
            raise RuntimeError("broken at seed 4")

    def act(self, obs):
        import time
        time.sleep(600)
'''


@pytest.fixture
def policy_module(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> str:
    """A policy module the spawned workers import too (through PYTHONPATH)."""
    directory = tmp_path / "policies"
    directory.mkdir()
    (directory / "gth_test_policies.py").write_text(CHUNK_POLICY, encoding="utf-8")
    monkeypatch.syspath_prepend(str(directory))
    monkeypatch.setenv("PYTHONPATH", str(directory))
    yield "gth_test_policies"
    sys.modules.pop("gth_test_policies", None)


def test_chunks_play_one_action_per_step(
    testcore_path: Path, testcore_data: Path, tmp_path: Path, policy_module: str
):
    out = tmp_path / "out"
    path = evaluate(
        protocol(sticky_prob=0.0),
        f"{policy_module}:Chunks",
        out,
        testcore_data,
        policy_kwargs={"chunk": 3, "checkpoints": {"4": "cd" * 32}},
        record=Recording(license="CC0-1.0", checks_every=8),
        jobs=2,
        core_path=testcore_path,
    )
    run_info = json.loads(path.read_text())
    # One checkpoint per policy seed (a training run each), kept per seed and per row.
    assert run_info["policy"]["seeds"] == {
        "3": {"id": "chunks-v1", "checkpoint_sha256": "ab" * 32},
        "4": {"id": "chunks-v1", "checkpoint_sha256": "cd" * 32},
    }
    assert run_info["policy"]["kwargs"] == {"chunk": 3, "checkpoints": {"4": "cd" * 32}}
    # A call per chunk of three steps (the last one cut short by the episode's end).
    episodes = rows(out)
    assert run_info["policy_calls"] == sum(-(-row["steps"] // 3) for row in episodes)
    # Up and shot, then shot: the shot slot only, of the four the test core maps.
    assert {tuple(row["mapped_slots_used"]) for row in episodes} == {("shot",)}
    for row in episodes:
        checkpoint = {3: "ab" * 32, 4: "cd" * 32}[row["policy_seed"]]
        assert row["checkpoint_sha256"] == checkpoint
        meta = tomllib.loads((out / row["recording"] / "meta.toml").read_text(encoding="utf-8"))
        assert meta["player"] == f"chunks-v1 checkpoint={checkpoint} seed={row['policy_seed']}"
    check_results(out)


def test_atari_actions_are_mapped_onto_the_pad(
    testcore_path: Path, testcore_data: Path, tmp_path: Path, policy_module: str
):
    # ALE's UPFIRE, FIRE, NOOP map to the flat actions Chunks plays (docs/actions.md), so the
    # two runs play the same episodes.
    for name, action_set in (("Chunks", "harness"), ("Atari", "atari_full")):
        run(
            testcore_path,
            testcore_data,
            tmp_path / name,
            f"{policy_module}:{name}",
            action_set=action_set,
        )
        check_results(tmp_path / name)
    played = [rows(tmp_path / name) for name in ("Chunks", "Atari")]
    for row in played[0] + played[1]:
        del row["policy_id"]
    assert played[0] == played[1]
    assert {tuple(row["mapped_slots_used"]) for row in played[1]} == {("shot",)}
    # The same through a minimal set, whose indices the policy plays.
    games = tmp_path / "games"
    (games / "testcore").mkdir(parents=True)
    toml = (GAMES_DIR / "testcore" / "game.toml").read_text(encoding="utf-8")
    first, rest = toml.split("\n", 1)
    minimal = f"{first}\natari_minimal_actions = [0, 1, 10]\n{rest}"
    (games / "testcore" / "game.toml").write_text(minimal, encoding="utf-8")
    loaded = GameConfig.load(games / "testcore" / "game.toml")
    assert loaded.atari_minimal_actions == [0, 1, 10], "a list of ints, not bytes"

    def play_minimal(out: str, actions: list[int]) -> None:
        evaluate(
            protocol(action_set="atari_minimal"),
            f"{policy_module}:Minimal",
            tmp_path / out,
            testcore_data,
            policy_kwargs={"actions": actions},
            record=None,
            jobs=2,
            core_path=testcore_path,
            games_dir=games,
        )

    play_minimal("Minimal", [2, 1, 0])
    check_results(tmp_path / "Minimal")
    minimal_rows = rows(tmp_path / "Minimal")
    for row in minimal_rows:
        del row["policy_id"]
    assert minimal_rows == played[0]
    with pytest.raises(EvaluationFailed, match=r"returned action 3, outside 0\.\.3"):
        play_minimal("past-minimal", [1, 3])
    # 18 is past ALE's last action.
    with pytest.raises(EvaluationFailed, match=r"returned action 18, outside 0\.\.18"):
        evaluate(
            protocol(action_set="atari_full"),
            f"{policy_module}:Atari",
            tmp_path / "past",
            testcore_data,
            policy_kwargs={"actions": [1, 18]},
            record=None,
            jobs=1,
            core_path=testcore_path,
        )


def test_policy_requirements_are_checked_against_the_protocol(
    testcore_path: Path, testcore_data: Path, tmp_path: Path, policy_module: str
):
    out = tmp_path / "out"
    with pytest.raises(ConfigError, match=r"frame_stack: the policy needs 3, the protocol has 2"):
        run(testcore_path, testcore_data, out, f"{policy_module}:Chunks", frame_stack=2)
    with pytest.raises(ConfigError, match=r"obs.size: the policy needs \(12, 10\)"):
        run(testcore_path, testcore_data, out, f"{policy_module}:Chunks", size=(16, 16))
    with pytest.raises(ConfigError, match="isn't a Policy"):
        run(testcore_path, testcore_data, out, f"{policy_module}:Wrong")
    with pytest.raises(ConfigError, match="has no callable Absent"):
        run(testcore_path, testcore_data, out, f"{policy_module}:Absent")
    with pytest.raises(ConfigError, match="cannot import"):
        run(testcore_path, testcore_data, out, "gth_no_such_module:Policy")
    assert not out.exists(), "a refused run writes nothing"


def test_a_policy_that_cannot_be_made_is_a_refused_run(
    testcore_path: Path,
    testcore_data: Path,
    tmp_path: Path,
    policy_module: str,
    capsys: pytest.CaptureFixture[str],
):
    out = tmp_path / "out"
    args = ["evaluate", str(testcore_data), "--protocol", str(write_protocol(tmp_path))]
    args += ["--jobs", "1", "--core-path", str(testcore_path)]
    args += ["--policy", f"{policy_module}:Missing", "--out", str(out)]
    kwargs = json.dumps({"checkpoint": str(tmp_path / "none.pt")})
    assert main([*args, "--policy-kwargs", kwargs]) == 2
    error = capsys.readouterr().err
    assert "policy seed 3 failed: FileNotFoundError" in error
    assert main([*args, "--policy-kwargs", '{"policy_seed": 1}']) == 2
    assert "policy_seed comes from the protocol" in capsys.readouterr().err
    assert not out.exists()


def test_a_failing_policy_fails_the_run_without_run_json(
    testcore_path: Path, testcore_data: Path, tmp_path: Path, policy_module: str
):
    out = tmp_path / "out"
    with pytest.raises(EvaluationFailed, match="testcore, policy seed 3, env seed 0: ValueError"):
        evaluate(
            protocol(),
            f"{policy_module}:Chunks",
            out,
            testcore_data,
            policy_kwargs={"chunk": 0},  # an empty chunk
            record=None,
            jobs=1,
            core_path=testcore_path,
        )
    assert not (out / "run.json").exists()
    with pytest.raises(ConfigError, match="not a finished evaluation"):
        check_results(out)


def test_the_first_failure_stops_the_run_at_once(
    testcore_path: Path, testcore_data: Path, tmp_path: Path, policy_module: str
):
    # Two at a time: seed 3 hangs, seed 4 fails. The failure is reported without waiting for
    # seed 3, which is terminated, and seed 5 never starts.
    started = tmp_path / "started"
    started.mkdir()
    begin = time.monotonic()
    with pytest.raises(EvaluationFailed, match="policy seed 4, env seed 0: RuntimeError"):
        evaluate(
            protocol(policy_seeds=(3, 4, 5), env_seeds=(0,)),
            f"{policy_module}:Slow",
            tmp_path / "out",
            testcore_data,
            policy_kwargs={"started": str(started)},
            record=None,
            jobs=2,
            core_path=testcore_path,
        )
    assert time.monotonic() - begin < 60
    assert sorted(path.name for path in started.iterdir()) == ["3", "4"]


@pytest.mark.parametrize(
    ("changes", "message"),
    [
        ({"max_episode_frames": 7}, r"noop_max \+ frameskip \(5 \+ 2\) must be below"),
        ({"games": ("testcore", "no-such-game")}, "no-such-game: no game config"),
        ({"score_var": "lives"}, "score_var 'lives' isn't one of its RAM variables"),
        ({"action_set": "atari_minimal"}, "testcore: action_set 'atari_minimal' needs the game's"),
        ({"reset": "ab" * 20}, "start.reset"),
        ({"crop": (0, 0, 10, 10)}, r"obs.crop \(0, 0, 10, 10\) differs"),
        ({"orientation": 0}, "obs.orientation 0 differs"),
    ],
)
def test_protocols_the_env_cannot_play_are_refused(
    testcore_path: Path, testcore_data: Path, tmp_path: Path, changes, message
):
    with pytest.raises(ConfigError, match=message):
        run(testcore_path, testcore_data, tmp_path / "out", **changes)
    assert not (tmp_path / "out").exists()


def test_a_missing_reset_state_is_refused(testcore_path: Path, testcore_data: Path, tmp_path: Path):
    games = tmp_path / "games"
    (games / "testcore").mkdir(parents=True)
    before, rest = (GAMES_DIR / "testcore" / "game.toml").read_text().split("[reset]")
    state = '[reset]\nstate = { file = "start.state", sha1 = "' + "ab" * 20 + '" }\n\n'
    toml = before + state + "[ram.score]" + rest.split("[ram.score]")[1]
    (games / "testcore" / "game.toml").write_text(toml, encoding="utf-8")
    with pytest.raises(ConfigError, match=r"testcore: no reset state .*start\.state"):
        check_protocol(
            protocol(), testcore_data, rom_dir=None, core_path=testcore_path, games_dir=games
        )
    with pytest.raises(ConfigError, match=r"testcore: start\.later_episodes 'chain' soft-resets"):
        check_protocol(
            protocol(later_episodes="chain"),
            testcore_data,
            rom_dir=None,
            core_path=testcore_path,
            games_dir=games,
        )


def test_the_games_own_crop_and_orientation_may_be_spelled_out(
    testcore_path: Path, testcore_data: Path
):
    games = check_protocol(
        protocol(crop=(0, 0, 320, 232), orientation=90),
        testcore_data,
        rom_dir=None,
        core_path=testcore_path,
        games_dir=GAMES_DIR,
    )
    assert list(games) == ["testcore"]


def test_a_maxpooled_gray_protocol_runs_and_replays(
    testcore_path: Path, testcore_data: Path, tmp_path: Path, policy_module: str
):
    text = PROTOCOL.replace("maxpool = false", "maxpool = true")
    text = text.replace("grayscale = false", "grayscale = true")
    assert loads(text).maxpool and loads(text).obs.grayscale
    out = tmp_path / "out"
    status = main(
        [
            "evaluate",
            str(testcore_data),
            "--protocol",
            str(write_protocol(tmp_path, text)),
            "--policy",
            f"{policy_module}:Gray",
            "--out",
            str(out),
            "--record",
            "--license",
            "CC0-1.0",
            "--checks-every",
            "8",
            "--jobs",
            "2",
            "--core-path",
            str(testcore_path),
        ]
    )
    assert status == 0  # Gray.act asserts its observations are one channel
    results = check_results(out)
    assert results["protocol"] == loads(text)
    assert results["run"]["policy_calls"] > 0 and len(results["episodes"]) == 8
    # The RGB policy refuses the gray protocol before anything runs.
    with pytest.raises(ConfigError, match="grayscale"):
        evaluate(
            loads(text),
            f"{policy_module}:Chunks",
            tmp_path / "refused",
            testcore_data,
            policy_kwargs={},
            record=None,
            jobs=1,
            core_path=testcore_path,
        )
    assert not (tmp_path / "refused").exists()


def test_results_are_never_overwritten(testcore_path: Path, testcore_data: Path, tmp_path: Path):
    out = tmp_path / "out"
    out.mkdir()
    (out / "old").write_text("x")
    with pytest.raises(ConfigError, match="never overwritten"):
        run(testcore_path, testcore_data, out)


def test_check_results_finds_edits(testcore_path: Path, testcore_data: Path, tmp_path: Path):
    out = tmp_path / "out"
    run(testcore_path, testcore_data, out)
    episodes = (out / "episodes.jsonl").read_text()
    lines = episodes.splitlines()

    (out / "episodes.jsonl").write_text("\n".join(lines[:-1]) + "\n")
    with pytest.raises(ConfigError, match="7 rows, not the protocol's 8"):
        check_results(out)

    row = json.loads(lines[0]) | {"truncated": True, "terminated": False, "frames": 30}
    (out / "episodes.jsonl").write_text("\n".join([json.dumps(row), *lines[1:]]) + "\n")
    with pytest.raises(ConfigError, match=r"episodes.jsonl:1: truncated exactly when"):
        check_results(out)

    (out / "episodes.jsonl").write_text(episodes)
    protocol_toml = out / "protocol.toml"
    protocol_toml.write_text(protocol_toml.read_text().replace("noop_max = 5", "noop_max = 4"))
    with pytest.raises(ConfigError, match=r"protocol_sha1 isn't protocol.toml's SHA-1"):
        check_results(out)


def test_cli_needs_a_protocol_and_record_options_together(
    testcore_data: Path, tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    with pytest.raises(SystemExit) as exit_info:
        main(["evaluate", str(testcore_data), "--policy", RANDOM, "--out", str(tmp_path / "o")])
    assert exit_info.value.code == 2
    assert "--protocol" in capsys.readouterr().err
    base = ["evaluate", str(testcore_data), "--policy", RANDOM, "--out", str(tmp_path / "o")]
    with pytest.raises(SystemExit):
        main([*base, "--protocol", "ours-v1"])
    assert "--jobs" in capsys.readouterr().err, "--jobs is required: it costs a policy each"
    base += ["--jobs", "1"]
    assert main([*base, "--protocol", "ours-v1", "--record"]) == 2
    assert "--license and --checks-every go with --record" in capsys.readouterr().err
    assert main([*base, "--protocol", "no-such-preset"]) == 2
    assert "cannot read the protocol" in capsys.readouterr().err


def test_preset_names_resolve_to_protocols_dir():
    assert evaluate_module.protocol_path("ours-v1").name == "ours-v1.toml"
    assert evaluate_module.protocol_path("ours-v1").is_file()
    assert evaluate_module.protocol_path("my/run.toml") == Path("my/run.toml")
