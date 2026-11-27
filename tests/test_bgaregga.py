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

import numpy as np

# The core's own picture size, so frames compare unresized.
OBS = (240, 320)
# Attract mode fades its palette in and out (frames 3001 and 3501 among them), and the state
# the check loads (conftest.py's STATE_STEP) is from a frame where the palette is changing.


def test_recordings_replay_and_states_load_alike_in_a_fresh_process(replay_and_cold_load):
    load = replay_and_cold_load("bgaregga", obs=OBS, settle_frames=1, settled_steps=None)
    # The cause, still there: unsettled, a fresh core draws the state's first frame with the
    # palette it starts with, not the one the recording process had (and a fresh process dupes
    # its first frame anyway, as every FBNeo game does: docs/performance.md).
    assert not np.array_equal(load.fresh_first_unsettled, load.warm_first)


def test_a_recording_of_play_replays_in_a_fresh_process(play_replays):
    play_replays("bgaregga")
