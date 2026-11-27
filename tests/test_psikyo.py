"""games/gunbird and games/s1945 (FBNeo's Psikyo driver) on the real core and the user's ROMs:
their recordings replay in a fresh process, and a savestate loaded into another core gives the
frames the live run did and, a few steps on, the live run's whole savestate (docs/performance.md,
"FBNeo and savestates"). Four things FBNeo's
savestates didn't hold make them depend on what the process ran before a load, each added by
a commit on our FBNeo fork (docs/cores.md, "Forks"):

- the sprite lists the driver copies out of sprite RAM at each vblank and draws two frames
  later (double-buffered sprites); without them the first two frames after a load show the
  sprites of whatever the process last ran, and settling can't help, since the settle frames
  buffer later sprite RAM than the saved run drew;
- the YM2610's resampling position and carried samples (Gunbird), and the LFO and envelope
  counters of its FM core; without them the chip's phases and volumes drift from the saved
  run's from the first frame after a load (the sound CPU could see it through the ADPCM flags);
- the YMF278B's render position and carried samples (Strikers 1945), which the scan used to
  zero on a load; without them its slots run ahead of the saved run's.

Settle frames stay at 1: FBNeo's libretro port repeats (dupes) the first frame a process
runs, since retro_get_system_av_info flags its video buffer for reallocation, so a fresh
process that loads a state and draws at once shows a cleared frame. The settle frame absorbs
it (docs/games.md, "Settle frames").

Skips without the ROM or the pinned core, as on CI (docs/testing.md); an FBNeo build without
the fork's commits isn't the pinned one, so it skips too."""

import numpy as np
import pytest

GAMES = ["gunbird", "s1945"]
# The core's own picture size, so frames compare unresized.
OBS = (224, 320)
# The frame the check's resets from the state show is the fifth after the load, past the two
# frames the sprite lists are drawn late; the first frame after the load (ColdLoad.warm_first)
# is where those show.
# Steps with no button held from the state on (conftest.py's STATE_STEP), after which the live run's
# whole savestate is compared with the loaded cores'. The sound chips' state (the YM2610's and
# YMF278B's carried samples, the FM core's counters) doesn't reach the frames or RAM a recording
# checks, so only the savestate shows it.
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
