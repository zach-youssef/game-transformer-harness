"""games/ket, games/ddp3 and games/ddpdojblk (FBNeo's IGS PGM driver; Black Label is a clone
inside ddp3.zip, content.in_parent) on the real core and the user's ROMs: their recordings
replay in a fresh process, and a savestate loaded into another core gives the frames the live
run did and, a few steps on, the live run's whole savestate (docs/performance.md, "FBNeo and
savestates"). One thing FBNeo's savestates didn't hold made them depend on what the process
ran before a load, added by a commit on our FBNeo fork (docs/cores.md, "Forks"): the CPUs'
cycle overshoot past each frame's budget, which the next frame starts by idling off
(pgm_run.cpp's nCyclesDone). Without it the first frame after a load ran the CPUs a few
cycles more or fewer than the saved run, their sync points moved, and the Z80 RAM, then the
68000 RAM, diverged within a few frames while the picture stayed the same for thousands
(without it, the attract-mode replays below desync in RAM, but a play recording may replay:
the whole savestate shows it on the first frame after a load).

Settle frames stay at 1 for a reason outside the driver: FBNeo's libretro port repeats
(dupes) the first frame a process runs, since retro_get_system_av_info flags its video
buffer for reallocation, so a fresh process that loads a state and draws at once shows a
cleared frame. The settle frame absorbs it (docs/games.md, "Settle frames"); the driver
rebuilds its palette from the loaded palette RAM, so it needs no settling of its own.

Skips without the ROM or the pinned core, as on CI (docs/testing.md); an FBNeo build without
the fork's commit isn't the pinned one, so it skips too."""

import subprocess
import sys
from pathlib import Path

import numpy as np
import pytest

from gtharness import _native, bench
from gtharness.env import GAMES_DIR, RetroEnv
from gtharness.record import Record

GAMES = ["ket", "ddp3", "ddpdojblk"]
FRAMESKIP = 4
# The core's own picture size, so frames compare unresized.
OBS = (224, 448)
SHOT = 1  # flat action: no direction, the shot slot (game.toml: a)
UP_SHOT = 1 * 16 + SHOT  # flat action: up and shot (docs/actions.md)
# Steps of the PGM BIOS boot and attract mode, which animates sprites throughout.
STEPS = 1000
# The step whose state the other cores load.
STATE_STEP = 750
# Steps with no button held from STATE_STEP on, after which the live run's whole savestate is
# compared with the loaded cores'. A wrong cycle carry shows there (in the CPUs' cycle
# counts and the Z80's registers) on the first frame after the load, before it reaches the
# frames or RAM a recording checks.
SETTLED_STEPS = 15


def no_reward(previous, current) -> float:
    """The game configs have no RAM variables yet (stubs until G5), so no reward reads one."""
    return 0.0


# Runs in a fresh process, so nothing the recording process ran reaches the core: replays the
# recording, then loads the state into a fresh environment and into fresh cores, settled and
# not, saving what each showed, and the settled core's savestate SETTLED_STEPS steps later.
FRESH = """
import sys
from pathlib import Path

import numpy as np

from gtharness import _native
from gtharness.env import RetroEnv
from gtharness.record import Replayer

def no_reward(previous, current):
    return 0.0

game_id = sys.argv[1]
recording, core_path, content, out = map(Path, sys.argv[2:6])
frameskip, height, width, settled_frames = map(int, sys.argv[6:10])
data = core_path.parent.parent
game = _native.GameConfig.load(sys.argv[10])
state = (out / "state").read_bytes()

replay = Replayer(
    recording, core_path, content, data / "system", height, width, game.crop,
    game.orientation, "area",
)
try:
    print(replay.run_to_end(), replay.frames)
finally:
    replay.close()

env = RetroEnv.from_game(
    game_id, data, frameskip=frameskip, obs_size=(height, width), action_format="flat",
    reward=no_reward, core_path=core_path,
)
try:
    observation, _ = env.reset(options={"state": state})
    np.save(out / "fresh-reset.npy", observation)
finally:
    env.close()

for settle in (game.settle_frames, 0):
    core = _native.Core(
        core_path, content, data / "system", game.core_options, settle, height, width,
        game.crop, game.orientation, "area",
    )
    try:
        core.unserialize(state)
        np.save(out / f"fresh-first-{settle}.npy", core.step(1, 0))
        if settle == game.settle_frames:
            core.step(settled_frames - 1, 0)
            (out / "fresh-settled-state").write_bytes(core.serialize())
    finally:
        core.close()
"""


@pytest.mark.parametrize("game_id", GAMES)
def test_recordings_replay_and_states_load_alike_in_a_fresh_process(
    game_id, rom, core, tmp_path: Path
):
    content = rom(game_id)
    core_path = core(game_id)
    data = core_path.parent.parent
    game_toml = GAMES_DIR / game_id / "game.toml"
    game = _native.GameConfig.load(game_toml)
    assert game.settle_frames == 1
    env = RetroEnv.from_game(
        game_id,
        data,
        frameskip=FRAMESKIP,
        obs_size=OBS,
        action_format="flat",
        reward=no_reward,
        core_path=core_path,
        record=Record(
            directory=tmp_path / "recordings",
            recorder="scripted",
            player="scripted",
            license="CC0-1.0",
            checks_every=1,
        ),
    )
    rng = np.random.default_rng(0)
    try:
        env.reset()
        for step in range(STEPS):
            if step == STATE_STEP:
                state = env.save_state()
            if STATE_STEP <= step < STATE_STEP + SETTLED_STEPS:
                observation, *_ = env.step(0)
                if step == STATE_STEP:
                    live = observation
                if step == STATE_STEP + SETTLED_STEPS - 1:
                    live_settled_state = env.save_state()
            else:
                env.step(int(rng.choice([0, SHOT, UP_SHOT])))
        # In this process, after everything above: the reset from the state shows what the
        # live run did. It ends the recording and starts another.
        observation, _ = env.reset(options={"state": state})
        np.testing.assert_array_equal(observation, live)
    finally:
        env.close()
    recording = env.recordings[0]

    # And this process's first frame after loading the state, with its history, for the fresh
    # process to match, and its savestate SETTLED_STEPS steps later.
    warm = _native.Core(
        core_path,
        content,
        data / "system",
        game.core_options,
        game.settle_frames,
        *OBS,
        game.crop,
        game.orientation,
        "area",
    )
    try:
        warm.step(3000, 0)
        warm.unserialize(state)
        warm_first = warm.step(1, 0)
        warm.step(SETTLED_STEPS * FRAMESKIP - 1, 0)
        warm_settled_state = warm.serialize()
    finally:
        warm.close()

    out = tmp_path / "fresh"
    out.mkdir()
    (out / "state").write_bytes(state)
    run = subprocess.run(
        [
            sys.executable,
            "-c",
            FRESH,
            game_id,
            str(recording),
            str(core_path),
            str(content),
            str(out),
            str(FRAMESKIP),
            *map(str, OBS),
            str(SETTLED_STEPS * FRAMESKIP),
            str(game_toml),
        ],
        capture_output=True,
        text=True,
    )
    # A desync raises DesyncError in the replay, naming its frame.
    assert run.returncode == 0, run.stderr
    replayed, frames = map(int, run.stdout.split())
    assert replayed == frames == (STEPS + 1) * FRAMESKIP, "the reset's frames, then the steps'"

    np.testing.assert_array_equal(np.load(out / "fresh-reset.npy"), live)
    np.testing.assert_array_equal(np.load(out / "fresh-first-1.npy"), warm_first)
    # The whole savestate after the load, in this process and in the fresh one, is the live
    # run's.
    assert warm_settled_state == live_settled_state, "this process's state after the load"
    fresh_settled_state = (out / "fresh-settled-state").read_bytes()
    assert fresh_settled_state == live_settled_state, "the fresh process's state after the load"
    # Why settling stays on: unsettled, a fresh FBNeo process dupes its first frame, so the
    # frame it shows right after the load is the cleared one, not the game's.
    fresh_first_unsettled = np.load(out / "fresh-first-0.npy")
    assert not np.array_equal(fresh_first_unsettled, warm_first)
    assert not fresh_first_unsettled.any()


@pytest.mark.parametrize("game_id", GAMES)
def test_a_recording_of_play_replays_in_a_fresh_process(game_id, rom, core, tmp_path: Path):
    """The benchmark's determinism check (docs/performance.md): it coins and starts a game, so
    the recording covers play, not only attract mode."""
    content = rom(game_id)
    data = core(game_id).parent.parent
    target = bench.game_target(game_id, data, content.parent)
    result = bench.determinism(target, 4000, tmp_path, seed=0)
    assert result["desync"] is None, result["desync"]
    assert result["replayed"] == result["frames"]
