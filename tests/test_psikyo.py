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

Their sprite-list labels (docs/recording-format.md, "Sprite labels") are checked against the
drawn frames, frame-exactly, and `gth capture`'s scripts (docs/recording-format.md, "Scripted
captures") export with them.

Skips without the ROM or the pinned core, as on CI (docs/testing.md); an FBNeo build without
the fork's commits isn't the pinned one, so it skips too."""

import itertools

import numpy as np
import pytest

from gtharness.capture import CREDIT_EVERY, PULSE, capture, player_name
from gtharness.env import RetroEnv
from gtharness.record import Record
from gtharness.shards import SPRITE_VISIBLE, Shard, ShardOptions, export_shards

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


# The sprite-label check's run: from the reset, random actions at one frame a step, through
# attract mode's static screens (credits, titles), where sprites move over a still background.
SPRITE_FRAMES = 2400


def _boxes(shard, frame_index: int, shape: tuple[int, int]) -> np.ndarray:
    """The union of frame index ``frame_index``'s visible sprite boxes."""
    mask = np.zeros(shape, bool)
    for sprite in shard.sprites(frame_index):
        if sprite["flags"] & SPRITE_VISIBLE:
            x, y = max(int(sprite["x"]), 0), max(int(sprite["y"]), 0)
            mask[y : max(int(sprite["y"]) + int(sprite["height"]), 0),
                 x : max(int(sprite["x"]) + int(sprite["width"]), 0)] = True  # fmt: skip
    return mask


@pytest.mark.parametrize("game_id", GAMES)
def test_sprite_labels_are_the_frame_s_own(game_id, rom, core, tmp_path):
    """Each frame's sprite list is the one that frame draws, not its neighbour's: where the
    background stands still, every pixel that changes from frame f - 1 to f lies in a sprite
    box of one of the two frames. Shifting the labels a frame either way leaves the leading or
    trailing edge of each moving sprite uncovered, so the right alignment covers best by a
    clear margin (docs/recording-format.md, "Sprite labels")."""
    content = rom(game_id)
    path = core(game_id)
    data = path.parent.parent
    env = RetroEnv.from_game(
        game_id,
        data,
        frameskip=1,
        obs_size=OBS,
        action_format="flat",
        reward=lambda previous, current: 0.0,
        sticky_prob=0.0,
        max_episode_frames=None,
        noop_max=0,
        maxpool=False,
        grayscale=False,
        rom_dir=content.parent,
        core_path=path,
        record=Record(
            directory=tmp_path / "rec",
            recorder="scripted",
            player="random",
            license="CC0-1.0",
            checks_every=60,
        ),
    )
    rng = np.random.default_rng(5)
    try:
        env.reset()
        for _ in range(SPRITE_FRAMES):
            env.step(int(rng.integers(0, env.config.n_actions)))
    finally:
        env.close()
    options = ShardOptions(
        obs_size=OBS,
        crop=None,
        orientation=0,
        frameskip="recorded",
        filter="all",
        chunk_samples=256,
        interpolation="area",
        sprites=True,
    )
    (result,) = export_shards(
        env.recordings, tmp_path / "shards", data, options, jobs=1, rom_dir=content.parent,
        core_path=path,
    )  # fmt: skip
    assert result.error is None, result.error
    with Shard(result.shard) as shard:
        # Unrotated and uncropped, the observation is the native screen.
        assert shard.sprite_hw == "psikyo" and shard.sprite_screen == (OBS[1], OBS[0])
        frames = np.concatenate([shard.frames(c) for c in range(shard.frame_chunks)])
        frames = frames.astype(np.int16)
        shown = shard.episode_frame.astype(np.int64) - 1  # the frame index each sample shows
        assert (np.diff(shown) == 1).all()
        cache: dict[int, np.ndarray] = {}

        def boxes(frame_index: int) -> np.ndarray:
            if frame_index not in cache:
                cache[frame_index] = _boxes(shard, frame_index, OBS)
            return cache[frame_index]

        uncovered = {-1: 0, 0: 0, 1: 0}
        used = 0
        for i in range(2, len(frames) - 2):
            f = int(shown[i])
            changed = np.abs(frames[i] - frames[i - 1]).sum(-1) > 0
            near = boxes(f - 2) | boxes(f - 1) | boxes(f) | boxes(f + 1)
            if not changed.any() or (changed & ~near).any():
                continue  # nothing moved, or the background did
            used += 1
            for shift in uncovered:
                covered = boxes(f - 1 + shift) | boxes(f + shift)
                uncovered[shift] += int((changed & ~covered).sum())
    assert used >= 200, f"only {used} frames with a still background"
    # Measured 1.66x (gunbird) and 1.77x (s1945) at the pin; one frame late or early is worse.
    assert uncovered[0] * 1.3 < min(uncovered[-1], uncovered[1]), uncovered


CAPTURE_FRAMES = 2400  # past random-play's first game start (a credit cycle every 480 frames)


@pytest.mark.parametrize("game_id", GAMES)
def test_captures_export_with_sprite_labels(game_id, rom, core, tmp_path):
    """`gth capture`'s scripts (docs/recording-format.md, "Scripted captures") record the game
    from its reset, and the recordings replay without desync into shards with every frame's
    sprite list, losing only the coin and start presses' samples. random-play really plays:
    attract mode ignores input, so two seeds' sprites diverge only once a credit starts a game."""
    content = rom(game_id)
    path = core(game_id)
    data = path.parent.parent
    runs = [("attract", None), ("random-play", 0), ("random-play", 1)]
    recordings = [
        capture(
            game_id,
            data,
            tmp_path / "rec",
            script=script,
            frames=CAPTURE_FRAMES,
            frameskip=1,
            seed=seed,
            license="CC0-1.0",
            checks_every=60,
            rom_dir=content.parent,
            core_path=path,
        )
        for script, seed in runs
    ]
    options = ShardOptions(
        obs_size=(112, 160),
        crop=None,
        orientation=0,
        frameskip=1,
        filter="all",
        chunk_samples=256,
        interpolation="area",
        sprites=True,
    )
    results = export_shards(
        recordings, tmp_path / "shards", data, options, jobs=3, rom_dir=content.parent,
        core_path=path,
    )  # fmt: skip
    sprites = []
    for (script, seed), result in zip(runs, results, strict=True):
        assert result.error is None, result.error
        with Shard(result.shard) as shard:
            assert shard.meta["recording"]["player"] == player_name(script, seed)
            assert shard.meta["frames"] == CAPTURE_FRAMES
            assert len(shard.sprite_offsets) == CAPTURE_FRAMES + 1
            # Coin and start presses label no sample (the last observation is no sample either).
            presses = int((~shard.frame_labelled).sum())
            assert shard.meta["samples"] == CAPTURE_FRAMES - 1 - presses
            expected = 0 if script == "attract" else 5 * 2 * PULSE  # five credit cycles
            assert presses == expected
            drawn = np.diff(shard.sprite_offsets.astype(np.int64)) > 0
            assert drawn.mean() > 0.5, "most frames draw sprites"
            records = np.concatenate([shard.sprite_chunk(c) for c in range(shard.sprite_chunks)])
            offsets = shard.sprite_offsets.astype(np.int64)
            sprites.append([records[a:b].tobytes() for a, b in itertools.pairwise(offsets)])

    def differing(a, b):
        return sum(x != y for x, y in zip(a[CREDIT_EVERY:], b[CREDIT_EVERY:], strict=True))

    # The first frames, before a press shows, are the same attract mode in every capture.
    assert sprites[0][: 2 * PULSE] == sprites[1][: 2 * PULSE] == sprites[2][: 2 * PULSE]
    after = CAPTURE_FRAMES - CREDIT_EVERY
    # Measured at the pin, of 1,920 frames: from attract 1,908 (gunbird) and 1,907 (s1945) for
    # both seeds; seed 0 from seed 1, 1,858 and 1,837. Start replaced by a coin gave 154.
    left_attract = [differing(sprites[0], sprites[other]) for other in (1, 2)]
    assert min(left_attract) > after / 2, f"random-play left attract mode: {left_attract}"
    # A coin alone (no game started) still diverges from attract; only play tells two seeds apart.
    seeds = differing(sprites[1], sprites[2])
    assert seeds > after / 2, f"the seeds' inputs reach a game in play: {seeds} of {after} frames"
