"""Training shards (docs/recording-format.md, "Training shards"): testcore recordings exported
with `export_shards` and read back with `Shard` and `batches` give the environment's own
observations, each with the action the env was given next."""

import json
import zipfile
from pathlib import Path

import numpy as np
import pytest

from gtharness._native import GameConfig
from gtharness.cli import main
from gtharness.env import GAMES_DIR, RetroEnv
from gtharness.errors import ConfigError, HarnessError
from gtharness.record import Record
from gtharness.rewards import score_delta
from gtharness.shards import (
    _EXIT_HOOK,
    SPRITE_DTYPE,
    SPRITE_VISIBLE,
    Shard,
    ShardOptions,
    batches,
    export_shards,
)

FRAMESKIP = 3
STEPS = 150


def options(**changes) -> ShardOptions:
    fields = {
        "obs_size": (60, 80),
        "crop": "game",
        "orientation": "game",
        "frameskip": "recorded",
        "filter": "all",
        "chunk_samples": 32,
        "interpolation": "area",
        "sprites": False,
    } | changes
    return ShardOptions(**fields)


def play(
    testcore_path: Path,
    testcore_data: Path,
    out: Path,
    episodes: int,
    idle: int,
    games_dir: Path = GAMES_DIR,
) -> tuple[list[Path], list[list[np.ndarray]], list[list[tuple[int, int]]]]:
    """Records ``episodes`` episodes of ``idle`` neutral steps, then random flat actions.
    Returns the recordings, each episode's observations (reset's first) and its actions as
    ``(direction, buttons)`` (neutral for the reset)."""
    env = RetroEnv.from_game(
        "testcore",
        testcore_data,
        frameskip=FRAMESKIP,
        obs_size=(60, 80),
        action_format="flat",
        reward=score_delta,
        sticky_prob=0.0,
        max_episode_frames=None,
        noop_max=0,
        core_path=testcore_path,
        games_dir=games_dir,
        record=Record(
            directory=out, recorder="scripted", player="random", license="CC0-1.0", checks_every=60
        ),
    )
    rng = np.random.default_rng(9)
    observations: list[list[np.ndarray]] = []
    actions: list[list[tuple[int, int]]] = []
    try:
        for _ in range(episodes):
            obs, _ = env.reset()
            observations.append([obs])
            actions.append([(0, 0)])
            for step in range(STEPS):
                action = 0 if step < idle else int(rng.integers(1, env.config.n_actions))
                obs, *_ = env.step(action)
                observations[-1].append(obs)
                actions[-1].append(env.config.decode(action))
    finally:
        env.close()
    return env.recordings, observations, actions


def test_a_shard_holds_the_env_frames_and_actions(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    paths, observations, actions = play(testcore_path, testcore_data, tmp_path / "rec", 2, 0)
    out = tmp_path / "shards"
    results = export_shards(paths, out, testcore_data, options(), jobs=2, core_path=testcore_path)
    assert [r.error for r in results] == [None, None]
    for result, path, episode, episode_actions in zip(
        results, paths, observations, actions, strict=True
    ):
        assert result.recording == path
        assert result.shard == out / path.name.replace(".gthrec", ".npz")
        assert result.summary is not None
        # Observation j (reset's is 0) with the action after it; the last has none.
        assert result.summary["samples"] == STEPS
        assert result.summary["frames"] == FRAMESKIP * (STEPS + 1)
        assert result.summary["bytes"] == result.shard.stat().st_size
        with Shard(result.shard) as shard:
            assert len(shard) == STEPS
            assert shard.meta["source_id"] == path.name
            assert shard.meta["frameskip"] == FRAMESKIP
            assert shard.meta["obs_size"] == [60, 80]
            assert (shard.meta["orientation"], shard.meta["crop"]) == (90, [0, 0, 320, 232])
            assert shard.meta["harness"]["version"]
            frames = np.concatenate([shard.frames(c) for c in range(shard.frame_chunks)])
            assert shard.frame_chunks == 5 and frames.shape == (STEPS, 60, 80, 3)
            np.testing.assert_array_equal(frames, np.stack(episode[:STEPS]))
            assert list(zip(shard.direction, shard.buttons, strict=True)) == episode_actions[1:]
            np.testing.assert_array_equal(
                shard.episode_frame, FRAMESKIP * np.arange(1, STEPS + 1, dtype=np.uint64)
            )
            np.testing.assert_array_equal(shard.segment, np.zeros(STEPS, np.uint32))
            np.testing.assert_array_equal(shard.confidence, np.ones(STEPS, np.float32))
            assert shard.ram.shape == (STEPS, len(shard.meta["ram_names"]))
            assert shard.ram.dtype == np.int64
            # Every frame's action: a step holds one for all its frames.
            per_frame = [a for a in episode_actions for _ in range(FRAMESKIP)]
            assert shard.meta["frames"] == len(per_frame)
            labels = list(zip(shard.frame_direction, shard.frame_buttons, strict=True))
            assert labels == per_frame
            assert shard.frame_labelled.dtype == np.bool_ and shard.frame_labelled.all()
            # A sample's label is the action of the frame after its observation.
            following = shard.episode_frame.astype(np.int64)
            np.testing.assert_array_equal(shard.direction, shard.frame_direction[following])
            np.testing.assert_array_equal(shard.buttons, shard.frame_buttons[following])

    # Shuffled batches hold every sample once, with its own frame and labels.
    seen = []
    shards = [r.shard for r in results]
    for batch in batches(shards, 40, rng=np.random.default_rng(1), buffer_chunks=3):
        assert "ram" not in batch
        assert len(batch["frame"]) <= 40
        for i in range(len(batch["frame"])):
            s, sample = int(batch["shard"][i]), int(batch["sample"][i])
            np.testing.assert_array_equal(batch["frame"][i], observations[s][sample])
            label = (int(batch["direction"][i]), int(batch["buttons"][i]))
            assert label == actions[s][sample + 1]
            assert batch["segment"][i] == 0
            seen.append((s, sample))
    assert sorted(seen) == [(s, i) for s in range(2) for i in range(STEPS)]
    assert seen != sorted(seen)
    # In order without an rng.
    ordered = list(batches(shards, 64, rng=None, buffer_chunks=1))
    assert [len(b["frame"]) for b in ordered] == [64, 64, 64, 64, 44]
    assert [int(s) for b in ordered for s in b["sample"]][:3] == [0, 1, 2]


def test_resampling_and_the_playing_filter(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    idle = 10
    paths, observations, actions = play(testcore_path, testcore_data, tmp_path / "rec", 1, idle)
    every3 = options(frameskip=FRAMESKIP, chunk_samples=1000)
    (all_steps,) = export_shards(
        paths, tmp_path / "all", testcore_data, every3, jobs=1, core_path=testcore_path
    )
    config = GameConfig.load(paths[0] / "game.toml")
    with Shard(all_steps.shard) as shard:
        names = shard.meta["ram_names"]
        terminated = [
            config.terminated(dict(zip(names, map(int, row), strict=True))) for row in shard.ram
        ]
    # Every 3 frames is the recorded rate. The playing filter keeps from observation `idle`,
    # which the first input answers, through game over: the first terminate after one that
    # wasn't (before the first input, the test core's lives may still read 0).
    first = idle
    armed = next(j for j in range(first, STEPS) if not terminated[j])
    game_over = next(j for j in range(armed, STEPS) if terminated[j])
    kept = game_over + 1 - first
    playing = options(frameskip=FRAMESKIP, filter="playing", chunk_samples=1000)
    (result,) = export_shards(
        paths, tmp_path / "playing", testcore_data, playing, jobs=1, core_path=testcore_path
    )
    assert result.summary is not None
    assert (result.summary["samples"], result.summary["filtered"]) == (kept, STEPS - kept)
    with Shard(result.shard) as shard:
        assert shard.meta["filter"] == "playing" and shard.meta["terminated"]
        np.testing.assert_array_equal(
            shard.frames(0), np.stack(observations[0][first : first + kept])
        )
        labels = list(zip(shard.direction, shard.buttons, strict=True))
        assert labels[0] != (0, 0)
        assert labels == actions[0][first + 1 : first + kept + 1]
        np.testing.assert_array_equal(shard.segment, np.zeros(kept, np.uint32))
    with Shard(all_steps.shard) as shard:
        assert shard.meta["terminated"] == terminated[-1]

    # Every 6 frames: an observation every second recorded step, labelled with the action of
    # the frame right after it (the next step's), not the one the observation ends.
    out6 = tmp_path / "shards6"
    (result,) = export_shards(
        paths, out6, testcore_data, options(frameskip=6), jobs=1, core_path=testcore_path
    )
    with Shard(result.shard) as shard:
        assert len(shard) == (STEPS + 1) // 2
        assert shard.meta["frameskip"] == 6
        frames = np.concatenate([shard.frames(c) for c in range(shard.frame_chunks)])
        np.testing.assert_array_equal(frames, np.stack(observations[0][1::2][: len(shard)]))
        labels = list(zip(shard.direction, shard.buttons, strict=True))
        assert labels == actions[0][2::2][: len(shard)]


def test_a_failed_recording_leaves_no_shard_and_the_rest_export(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    paths, *_ = play(testcore_path, testcore_data, tmp_path / "rec", 2, 0)
    (paths[1] / "checks.bin").write_bytes(b"not zstd")
    out = tmp_path / "shards"
    good, bad = export_shards(paths, out, testcore_data, options(), jobs=2, core_path=testcore_path)
    assert good.error is None and good.shard.exists()
    assert bad.summary is None and bad.error is not None and "checks.bin" in bad.error
    assert sorted(p.name for p in out.iterdir()) == [good.shard.name]
    # Existing shards are never replaced.
    with pytest.raises(ConfigError, match="already exist"):
        export_shards(paths[:1], out, testcore_data, options(), jobs=1, core_path=testcore_path)


def test_a_worker_that_dies_fails_only_its_recording(
    testcore_path: Path, testcore_data: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    paths, *_ = play(testcore_path, testcore_data, tmp_path / "rec", 3, 0)
    # The hook makes the worker of the middle recording write a partial shard and exit, as a
    # core that crashes mid-export would.
    monkeypatch.setenv(_EXIT_HOOK, paths[1].name)
    out = tmp_path / "shards"
    results = export_shards(paths, out, testcore_data, options(), jobs=2, core_path=testcore_path)
    assert [r.error is None for r in results] == [True, False, True]
    assert results[1].error is not None and "BrokenProcessPool" in results[1].error
    assert sorted(p.name for p in out.iterdir()) == sorted(
        r.shard.name for r in results if r.error is None
    )


@pytest.mark.parametrize(
    ("change", "message"),
    [
        ({"obs_size": (0, 80)}, "obs height"),
        ({"obs_size": (60, 2**32)}, "obs width"),
        ({"crop": (0, 0, 0, 232)}, "crop width"),
        ({"crop": (-1, 0, 320, 232)}, "crop x"),
        ({"crop": "wide"}, "crop"),
        ({"orientation": 180}, "orientation"),
        ({"frameskip": 0}, "frameskip"),
        ({"frameskip": True}, "frameskip"),
        ({"filter": "some"}, "filter"),
        ({"chunk_samples": 0}, "chunk_samples"),
        ({"interpolation": "cubic"}, "interpolation"),
        ({"sprites": "yes"}, "sprites"),
    ],
)
def test_options_are_checked_before_exporting(change: dict, message: str):
    with pytest.raises(ConfigError, match=message):
        options(**change)


def test_gth_export_shards(
    testcore_path: Path,
    testcore_data: Path,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
):
    paths, *_ = play(testcore_path, testcore_data, tmp_path / "rec", 1, 0)
    out = tmp_path / "shards"
    args = [
        "export-shards",
        str(testcore_data),
        str(paths[0]),
        "--out",
        str(out),
        "--obs-size",
        "30",
        "40",
        "--crop",
        "0",
        "0",
        "320",
        "232",
        "--orientation",
        "0",
        "--frameskip",
        "recorded",
        "--filter",
        "all",
        "--chunk-samples",
        "64",
        "--sprites",
        "no",
        "--jobs",
        "1",
        "--core-path",
        str(testcore_path),
    ]
    assert main(args) == 0
    assert f"{STEPS} samples" in capsys.readouterr().out
    with Shard(out / paths[0].name.replace(".gthrec", ".npz")) as shard:
        assert shard.obs_size == (30, 40) and shard.meta["orientation"] == 0
        assert shard.frames(0).shape == (64, 30, 40, 3)
    assert main(args) == 2
    assert "already exist" in capsys.readouterr().err
    bad_crop = args.copy()
    bad_crop[bad_crop.index("--crop") + 1 : bad_crop.index("--orientation")] = ["wide"]
    assert main(bad_crop) == 2
    assert "--crop" in capsys.readouterr().err
    for option in ("--frameskip", "--chunk-samples"):
        zero = args.copy()
        zero[zero.index(option) + 1] = "0"
        assert main(zero) == 2
        assert option.removeprefix("--").replace("-", "_") in capsys.readouterr().err
    with pytest.raises(SystemExit):
        main(args[: args.index("--filter")])  # The filter is required.


def sprite_games(tmp_path: Path) -> Path:
    """A games directory whose testcore game has the test core's sprite list on (docs/
    libretro.md, "Test core")."""
    text = (GAMES_DIR / "testcore" / "game.toml").read_text(encoding="utf-8")
    text = text.replace("\n[content]", 'sprite_hw = "testcore"\n\n[content]', 1)
    text += 'gth_testcore_sprites = "on"\n'
    game = tmp_path / "games" / "testcore" / "game.toml"
    game.parent.mkdir(parents=True)
    game.write_text(text, encoding="utf-8")
    assert GameConfig.load(game).sprite_hw == "testcore"
    return tmp_path / "games"


def test_a_shard_holds_every_frame_s_sprite_list(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    games = sprite_games(tmp_path)
    paths, *_ = play(testcore_path, testcore_data, tmp_path / "rec", 1, 0, games)
    shards = {}
    for frameskip in ("recorded", 5):
        sprites = options(frameskip=frameskip, sprites=True)
        (result,) = export_shards(
            paths,
            tmp_path / f"s{frameskip}",
            testcore_data,
            sprites,
            jobs=1,
            core_path=testcore_path,
        )
        assert result.error is None and result.summary is not None
        shards[frameskip] = result
    with Shard(shards["recorded"].shard) as shard, Shard(shards[5].shard) as other:
        frames = int(shard.meta["frames"])
        assert (shard.sprite_hw, shard.sprite_screen) == ("testcore", (320, 240))
        offsets = shard.sprite_offsets
        assert len(offsets) == frames + 1 and offsets[0] == 0
        assert int(offsets[-1]) == shards["recorded"].summary["sprites"]
        records = np.concatenate([shard.sprite_chunk(c) for c in range(shard.sprite_chunks)])
        assert records.dtype == SPRITE_DTYPE and len(records) == offsets[-1]
        # Every frame draws the square's marker first, 4x4 and on screen.
        for frame in (0, 1, frames // 2, frames - 1):
            list_ = shard.sprites(frame)
            np.testing.assert_array_equal(list_, records[offsets[frame] : offsets[frame + 1]])
            assert (int(list_[0]["width"]), int(list_[0]["height"])) == (4, 4)
            assert list_[0]["flags"] & SPRITE_VISIBLE
        # The sprite index is per replayed frame, whatever the observation rate.
        assert len(other) != len(shard)
        np.testing.assert_array_equal(other.sprite_offsets, offsets)
        np.testing.assert_array_equal(other.sprites(frames - 1), shard.sprites(frames - 1))
        with pytest.raises(IndexError):
            shard.sprites(frames)

    # A game without sprite_hw is refused before anything is exported.
    plain, *_ = play(testcore_path, testcore_data, tmp_path / "plain", 1, 0)
    with pytest.raises(ConfigError, match="no sprite_hw"):
        export_shards(
            plain,
            tmp_path / "none",
            testcore_data,
            options(sprites=True),
            jobs=1,
            core_path=testcore_path,
        )
    assert not (tmp_path / "none").exists()


def test_a_version_1_shard_reads_without_sprites(
    testcore_path: Path, testcore_data: Path, tmp_path: Path
):
    paths, *_ = play(testcore_path, testcore_data, tmp_path / "rec", 1, 0)
    (result,) = export_shards(
        paths, tmp_path / "shards", testcore_data, options(), jobs=1, core_path=testcore_path
    )
    # Version 1's meta.json: no sprite fields.
    old = tmp_path / "old.npz"
    with zipfile.ZipFile(result.shard) as source, zipfile.ZipFile(old, "w") as target:
        for item in source.infolist():
            data = source.read(item)
            if item.filename == "meta.json":
                meta = json.loads(data)
                meta["shard_version"] = 1
                for key in ("sprite_hw", "sprite_screen", "sprite_chunk_frames", "sprite_chunks"):
                    assert meta.pop(key) is None
                data = json.dumps(meta).encode()
            target.writestr(item, data)
    # Without sprite lists, in either version, every sprite accessor says so.
    for path in (old, result.shard):
        with Shard(path) as shard:
            assert len(shard) == STEPS and shard.sprite_hw is None
            assert shard.frames(0).shape[0] == 32
            for read in (
                lambda: shard.sprite_offsets,
                lambda: shard.sprite_screen,
                lambda: shard.sprite_chunks,
                lambda: shard.sprite_chunk(0),
                lambda: shard.sprites(0),
            ):
                with pytest.raises(HarnessError, match="no sprite lists"):
                    read()
