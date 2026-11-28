"""The policy protocol (docs/evaluation.md, "Policies"): requirements checked against a
protocol, a policy's identity, and the random policy's seeding."""

import dataclasses

import numpy as np
import pytest

from gtharness.policies import GameInfo, ObsRequirements, check_identity
from gtharness.policies.random import RandomPolicy
from gtharness.protocol import PROTOCOLS_DIR, load

ANY = ObsRequirements(
    size=None, grayscale=None, maxpool=None, frame_stack=None, frameskip=None, action_set=None
)


def test_requirements_name_each_field_the_protocol_breaks():
    ours = load(PROTOCOLS_DIR / "ours-v1.toml")
    assert ANY.mismatches(ours) == []
    exact = ObsRequirements(
        size=ours.obs.size,
        grayscale=ours.obs.grayscale,
        maxpool=ours.maxpool,
        frame_stack=ours.frame_stack,
        frameskip=ours.frameskip,
        action_set=ours.action_set,
    )
    assert exact.mismatches(ours) == []
    wrong = dataclasses.replace(
        exact, size=(84, 84), frame_stack=ours.frame_stack + 1, action_set="atari_full"
    )
    assert wrong.mismatches(ours) == [
        f"obs.size: the policy needs (84, 84), the protocol has {ours.obs.size!r}",
        f"frame_stack: the policy needs {ours.frame_stack + 1}, the protocol has"
        f" {ours.frame_stack}",
        f"action_set: the policy needs 'atari_full', the protocol has {ours.action_set!r}",
    ]


class Fake:
    def __init__(self, **attributes):
        self.id = "fake-v1"
        self.checkpoint_sha256 = None
        self.obs_requirements = ANY
        self.__dict__.update(attributes)

    def reset(self, seed, game):
        pass

    def act(self, obs):
        return 0


@pytest.mark.parametrize(
    ("attributes", "message"),
    [
        ({"id": ""}, "id must be a non-empty string"),
        ({"checkpoint_sha256": "AB" * 32}, "64 lowercase hex digits"),
        ({"checkpoint_sha256": "ab"}, "64 lowercase hex digits"),
        ({"obs_requirements": {}}, "must be an ObsRequirements"),
    ],
)
def test_identity_is_checked(attributes, message):
    assert check_identity(Fake(checkpoint_sha256="0f" * 32), "m:Fake").id == "fake-v1"
    with pytest.raises(TypeError, match=f"m:Fake: .*{message}"):
        check_identity(Fake(**attributes), "m:Fake")
    with pytest.raises(TypeError, match="isn't a Policy"):
        check_identity(object(), "m:Fake")


def test_random_policy_is_seeded_and_runs_on():
    game = GameInfo("testcore", 144, ("shot", "bomb", "aux1", "aux2"))
    obs = np.zeros((1, 4, 4, 3), np.uint8)
    policy = RandomPolicy(policy_seed=0)
    policy.reset(5, game)
    first = [policy.act(obs) for _ in range(50)]
    policy.reset(None, game)
    later = [policy.act(obs) for _ in range(50)]
    policy.reset(5, game)
    assert [policy.act(obs) for _ in range(50)] == first != later
    assert all(0 <= action < 144 for action in first + later)
    assert RandomPolicy.obs_requirements == dataclasses.replace(ANY, action_set="harness")
