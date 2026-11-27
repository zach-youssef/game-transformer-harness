"""games/bgaregga on the real core and the user's ROM: its recordings replay in a fresh
process, and a savestate loaded into a fresh core gives the frames the live run did
(docs/performance.md, "FBNeo and savestates"). Four things FBNeo's savestates don't hold
make both depend on what the process ran before a load:

- the palette FBNeo's Toaplan drivers draw with, which settling fixes (docs/games.md, "Settle
  frames");
- the YM2151's render position, which the first commit on our FBNeo fork adds to the state
  (docs/cores.md, "Forks"); without it an attract-mode replay desyncs in the sound CPU's RAM
  after about 800 frames;
- the scanline the Toaplan scanline register last returned, which the second commit adds;
  without it a replay of play (the benchmark's coin, start and random actions) desyncs by a
  line of the raster split after about 870 frames, with RAM still identical;
- the MSM6295's resampling position and the samples it interpolates between, which the third
  commit adds; without it the same replay desyncs in the Z80's RAM after about 3340 frames.

Skips without the ROM or the pinned core, as on CI (docs/testing.md); an FBNeo build without
the fork's commits isn't the pinned one, so it skips too."""

import subprocess
import sys
from pathlib import Path

import numpy as np

from gtharness import _native, bench
from gtharness.env import GAMES_DIR, RetroEnv
from gtharness.record import Record

FRAMESKIP = 4
# The core's own picture size, so frames compare unresized.
OBS = (240, 320)
SHOT = 1  # flat action: no direction, the shot slot (game.toml: a)
UP_SHOT = 1 * 16 + SHOT  # flat action: up and shot (docs/actions.md)
# Steps of attract mode, which fades its palette in and out (frames 3001 and 3501 among them).
STEPS = 1000
# The step whose state the fresh process loads: a frame where the palette is changing.
STATE_STEP = 750


def no_reward(previous, current) -> float:
    """game.toml has no RAM variables yet (a stub until G4), so no reward reads one."""
    return 0.0


# Runs in a fresh process, so nothing the recording process ran reaches the core: replays the
# recording, then loads the state into a fresh environment and into fresh cores, settled and
# not, saving what each showed.
FRESH = """
import sys
from pathlib import Path

import numpy as np

from gtharness import _native
from gtharness.env import RetroEnv
from gtharness.record import Replayer

def no_reward(previous, current):
    return 0.0

recording, core_path, content, out = map(Path, sys.argv[1:5])
frameskip, height, width = map(int, sys.argv[5:8])
data = core_path.parent.parent
game = _native.GameConfig.load(sys.argv[8])
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
    "bgaregga", data, frameskip=frameskip, obs_size=(height, width), action_format="flat",
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
    finally:
        core.close()
"""


def test_recordings_replay_and_states_load_alike_in_a_fresh_process(rom, core, tmp_path: Path):
    content = rom("bgaregga")
    core_path = core("bgaregga")
    data = core_path.parent.parent
    game_toml = GAMES_DIR / "bgaregga" / "game.toml"
    game = _native.GameConfig.load(game_toml)
    assert game.settle_frames == 1
    env = RetroEnv.from_game(
        "bgaregga",
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
                live, *_ = env.step(0)
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
    # process to match.
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
            str(recording),
            str(core_path),
            str(content),
            str(out),
            str(FRAMESKIP),
            *map(str, OBS),
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
    # The cause, still there: unsettled, a fresh core draws the state's first frame with the
    # palette it starts with, not the one the recording process had.
    assert not np.array_equal(np.load(out / "fresh-first-0.npy"), warm_first)


def test_a_recording_of_play_replays_in_a_fresh_process(rom, core, tmp_path: Path):
    """The benchmark's determinism check (docs/performance.md): it coins and starts a game, so
    the recording covers play, not only attract mode."""
    content = rom("bgaregga")
    data = core("bgaregga").parent.parent
    target = bench.game_target("bgaregga", data, content.parent)
    result = bench.determinism(target, 4000, tmp_path, seed=0)
    assert result["desync"] is None, result["desync"]
    assert result["replayed"] == result["frames"]
