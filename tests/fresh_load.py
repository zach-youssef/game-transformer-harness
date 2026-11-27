"""The fresh process of conftest.py's `replay_and_cold_load` check, run as a script so that
nothing the test's process ran reaches the core: replays the recording, then loads the state
into a fresh environment and into fresh cores, settled and not, saving what each showed and,
when asked, the settled core's savestate some frames later.

Arguments: game id, recording, core library, content, output directory (holding `state`),
frameskip, observation height and width, frames after the load to serialize at (0 for none)
and the game's game.toml."""

import sys
from pathlib import Path

import numpy as np

from gtharness import _native
from gtharness.env import RetroEnv
from gtharness.record import Replayer


def no_reward(previous, current) -> float:
    return 0.0


def main() -> None:
    game_id = sys.argv[1]
    recording, core_path, content, out = map(Path, sys.argv[2:6])
    frameskip, height, width, settled_frames = map(int, sys.argv[6:10])
    data = core_path.parent.parent
    game = _native.GameConfig.load(sys.argv[10])
    state = (out / "state").read_bytes()

    replay = Replayer(
        recording,
        core_path,
        content,
        data / "system",
        height,
        width,
        game.crop,
        game.orientation,
        "area",
    )
    try:
        print(replay.run_to_end(), replay.frames)
    finally:
        replay.close()

    env = RetroEnv.from_game(
        game_id,
        data,
        frameskip=frameskip,
        obs_size=(height, width),
        action_format="flat",
        reward=no_reward,
        sticky_prob=0.0,
        max_episode_frames=None,
        noop_max=0,
        core_path=core_path,
    )
    try:
        observation, _ = env.reset(options={"state": state})
        np.save(out / "fresh-reset.npy", observation)
    finally:
        env.close()

    for settle in (game.settle_frames, 0):
        core = _native.Core(
            core_path,
            content,
            data / "system",
            game.core_options,
            settle,
            height,
            width,
            game.crop,
            game.orientation,
            "area",
        )
        try:
            core.unserialize(state)
            np.save(out / f"fresh-first-{settle}.npy", core.step(1, 0, 0.0))
            if settle == game.settle_frames and settled_frames:
                core.step(settled_frames - 1, 0, 0.0)
                (out / "fresh-settled-state").write_bytes(core.serialize())
        finally:
            core.close()


if __name__ == "__main__":
    main()
