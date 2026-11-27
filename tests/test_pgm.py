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

import numpy as np
import pytest

GAMES = ["ket", "ddp3", "ddpdojblk"]
# The core's own picture size, so frames compare unresized.
OBS = (224, 448)
# Steps with no button held from the state on (conftest.py's STATE_STEP), after which the live run's
# whole savestate is compared with the loaded cores'. A wrong cycle carry shows there (in the CPUs'
# cycle counts and the Z80's registers) on the first frame after the load, before it reaches the
# frames or RAM a recording checks.
SETTLED_STEPS = 15


@pytest.mark.parametrize("game_id", GAMES)
def test_recordings_replay_and_states_load_alike_in_a_fresh_process(game_id, replay_and_cold_load):
    load = replay_and_cold_load(game_id, obs=OBS, settle_frames=1, settled_steps=SETTLED_STEPS)
    # Why settling stays on: unsettled, a fresh FBNeo process dupes its first frame, so the
    # frame it shows right after the load is the cleared one, not the game's.
    assert not np.array_equal(load.fresh_first_unsettled, load.warm_first)
    assert not load.fresh_first_unsettled.any()


@pytest.mark.parametrize("game_id", GAMES)
def test_a_recording_of_play_replays_in_a_fresh_process(game_id, play_replays):
    play_replays(game_id)
