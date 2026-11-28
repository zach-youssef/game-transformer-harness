"""The JAT policy (docs/evaluation.md, "Adapters"). In CI it runs with a stand-in model, so
neither torch nor the weights are needed: the checks of the checkpoint directory and the
arguments, the requirements it states, the image it hands the model and a `gth evaluate` run
on the test core. With the jat extra and ``GTH_JAT_CHECKPOINT`` (a hub snapshot directory) it
also plays the real model on the test core; otherwise that test skips."""

import hashlib
import importlib.util
import os
import sys
from pathlib import Path

import numpy as np
import pytest

from gtharness.evaluate import check_results, evaluate
from gtharness.policies import GameInfo, ObsRequirements, check_identity
from gtharness.policies.jat import CODE_SHA256, JatPolicy, sha256_file
from gtharness.protocol import loads

# 84x84 gray, max-pooled 4-frame stacks at frame skip 4 and ALE's 18 actions: what JAT needs.
PROTOCOL = """
backend = "libretro"
games = ["testcore"]
frameskip = 4
sticky_prob = 0.0
maxpool = true
frame_stack = 4
action_set = "atari_full"
max_episode_frames = 81
life_loss_terminal = false
reward_clip = "none"
score_var = "score"
episodes_per_seed = 2
env_seeds = [0]
policy_seeds = [0, 1]

[obs]
size = [84, 84]
crop = "game"
orientation = "game"
interpolation = "area"
grayscale = true

[start]
reset = "game"
noop_max = 0
later_episodes = "reload"
"""

STUB = '''
import hashlib
import numpy as np
from gtharness.policies.jat import CODE_SHA256, JatPolicy

class Backend:
    """Records what the policy hands the model; plays FIRE, then UPFIRE, then NOOP."""

    max_position_embeddings = 512

    def __init__(self):
        self.calls, self.seeds = [], []

    def reset(self, seed):
        self.seeds.append(seed)

    def act(self, image, n_actions, context_positions, deterministic):
        assert image.shape == (84, 84, 4) and image.dtype == np.uint8
        assert image.flags.c_contiguous
        self.calls.append((image.copy(), n_actions, context_positions, deterministic))
        return (1, 10, 0)[len(self.calls) % 3]

class StubJat(JatPolicy):
    code_sha256 = {
        name: hashlib.sha256(f"stub {name}".encode()).hexdigest() for name in CODE_SHA256
    }

    def _load(self, directory, *, device, threads):
        self.loaded = (directory, device, threads)
        return Backend()
'''

WEIGHTS = b"not really weights"


@pytest.fixture
def stub(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """The stand-in policy's module (importable by spawned workers too) and a checkpoint
    directory whose files match its `code_sha256`."""
    directory = tmp_path / "policies"
    directory.mkdir()
    (directory / "gth_jat_stub.py").write_text(STUB, encoding="utf-8")
    monkeypatch.syspath_prepend(str(directory))
    monkeypatch.setenv("PYTHONPATH", str(directory))
    checkpoint = tmp_path / "jat"
    checkpoint.mkdir()
    for name in CODE_SHA256:
        (checkpoint / name).write_text(f"stub {name}", encoding="utf-8")
    (checkpoint / "model.safetensors").write_bytes(WEIGHTS)
    import gth_jat_stub

    yield gth_jat_stub, checkpoint
    sys.modules.pop("gth_jat_stub", None)


def settings(checkpoint, **changes):
    return {
        "checkpoint": str(checkpoint),
        "action_set": "atari_full",
        "context_timesteps": 16,
        "deterministic": False,
        "device": "cpu",
        "threads": 2,
        **changes,
    }


def test_it_states_jats_observations_and_its_checkpoint(stub):
    module, checkpoint = stub
    policy = check_identity(module.StubJat(policy_seed=0, **settings(checkpoint)), "jat")
    assert policy.id == "jat-v1"
    assert policy.checkpoint_sha256 == hashlib.sha256(WEIGHTS).hexdigest()
    assert policy.obs_requirements == ObsRequirements(
        size=(84, 84),
        grayscale=True,
        maxpool=True,
        frame_stack=4,
        frameskip=4,
        action_set="atari_full",
    )
    assert policy.loaded == (checkpoint, "cpu", 2)
    minimal = module.StubJat(policy_seed=1, **settings(checkpoint, action_set="atari_minimal"))
    assert minimal.obs_requirements.action_set == "atari_minimal"
    assert minimal.checkpoint_sha256 == policy.checkpoint_sha256, "one checkpoint for all seeds"


def test_the_model_gets_the_stack_channels_last_oldest_first(stub):
    module, checkpoint = stub
    policy = module.StubJat(policy_seed=0, **settings(checkpoint, deterministic=True))
    policy.reset(7, GameInfo("testcore", 6, (0, 1, 3, 4, 11, 12), ("shot",)))
    policy.reset(None, GameInfo("testcore", 6, (0, 1, 3, 4, 11, 12), ("shot",)))
    obs = np.random.default_rng(0).integers(0, 256, (4, 84, 84, 1), dtype=np.uint8)
    assert policy.act(obs) == 10
    image, n_actions, context, deterministic = policy._backend.calls[0]
    for k in range(4):
        assert np.array_equal(image[:, :, k], obs[k, :, :, 0])
    # The game's actions (a minimal set of 6 here), 16 timesteps of two positions each.
    assert (n_actions, context, deterministic) == (6, 32, True)
    assert policy._backend.seeds == [7, None]
    with pytest.raises(ValueError, match=r"uint8\[4, 84, 84, 1\]"):
        policy.act(np.zeros((4, 84, 84, 3), np.uint8))


@pytest.mark.parametrize(
    ("changes", "error", "message"),
    [
        ({"action_set": "harness"}, ValueError, "action_set must be one of"),
        ({"device": "tpu"}, ValueError, "device must be one of"),
        ({"threads": 0}, ValueError, "threads must be an integer >= 1"),
        ({"deterministic": 1}, ValueError, "deterministic must be a bool"),
        ({"context_timesteps": 0}, ValueError, r"context_timesteps must be an integer in 1\.\.256"),
        ({"context_timesteps": 257}, ValueError, r"in 1\.\.256"),
    ],
)
def test_bad_settings_are_refused(stub, changes, error, message):
    module, checkpoint = stub
    with pytest.raises(error, match=message):
        module.StubJat(policy_seed=0, **settings(checkpoint, **changes))


def test_a_directory_with_other_code_or_no_weights_is_refused(stub):
    module, checkpoint = stub
    (checkpoint / "modeling_jat.py").write_text("print('other code')", encoding="utf-8")
    with pytest.raises(ValueError, match=r"modeling_jat\.py: SHA-256 differs"):
        module.StubJat(policy_seed=0, **settings(checkpoint))
    (checkpoint / "modeling_jat.py").unlink()
    with pytest.raises(FileNotFoundError, match=r"no modeling_jat\.py"):
        module.StubJat(policy_seed=0, **settings(checkpoint))
    (checkpoint / "modeling_jat.py").write_text("stub modeling_jat.py", encoding="utf-8")
    (checkpoint / "model.safetensors").unlink()
    with pytest.raises(FileNotFoundError, match=r"no model\.safetensors"):
        module.StubJat(policy_seed=0, **settings(checkpoint))
    # The real class checks the released code's hashes, not the stand-in's.
    with pytest.raises(ValueError, match="SHA-256 differs from revision 911dd7447b1c"):
        JatPolicy(policy_seed=0, **settings(checkpoint))


def test_it_plays_through_gth_evaluate(
    stub, testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    _, checkpoint = stub
    out = tmp_path / "out"
    evaluate(
        loads(PROTOCOL),
        "gth_jat_stub:StubJat",
        out,
        testcore_data,
        policy_kwargs=settings(checkpoint),
        record=None,
        jobs=2,
        core_path=testcore_path,
    )
    results = check_results(out)
    sha = hashlib.sha256(WEIGHTS).hexdigest()
    assert results["run"]["policy"]["seeds"] == {
        seed: {"id": "jat-v1", "checkpoint_sha256": sha} for seed in ("0", "1")
    }
    # FIRE and UPFIRE press shot (the test core scores while it is held).
    for row in results["episodes"]:
        assert row["mapped_slots_used"] == ["shot"]
        assert row["score"] > 0


REAL = os.environ.get("GTH_JAT_CHECKPOINT")


@pytest.mark.skipif(
    importlib.util.find_spec("torch") is None or importlib.util.find_spec("transformers") is None,
    reason="needs the jat extra (uv sync --extra jat)",
)
@pytest.mark.skipif(not REAL, reason="needs GTH_JAT_CHECKPOINT, a jat-project/jat snapshot")
def test_the_real_model_plays_the_test_core(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    episodes = []
    for threads in (2, 1):
        out = tmp_path / f"threads-{threads}"
        evaluate(
            loads(PROTOCOL),
            "gtharness.policies.jat:JatPolicy",
            out,
            testcore_data,
            policy_kwargs=settings(REAL, threads=threads),
            record=None,
            jobs=1,
            core_path=testcore_path,
        )
        results = check_results(out)
        run = results["run"]
        assert {seed["checkpoint_sha256"] for seed in run["policy"]["seeds"].values()} == {
            sha256_file(Path(REAL) / "model.safetensors")
        }
        assert run["steps"] > 0 and np.isfinite(run["policy_ms_per_step"])
        episodes.append(results["episodes"])
    # The seeds fix the sampled actions, whatever the thread count: the same episodes.
    assert episodes[0] == episodes[1]
