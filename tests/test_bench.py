"""`gth bench` (docs/performance.md) on the test core with tiny counts, so CI checks every
measurement runs and reports; the real cores' numbers come from manual runs, and
test_bench_real_rom skips without a ROM directory."""

import hashlib
import json
import shutil
import struct
import tomllib
from collections.abc import Callable
from pathlib import Path

import numpy as np
import pytest

from gtharness import _native, bench, cli
from gtharness.env import GAMES_DIR, core_library

TINY = [
    "--steps",
    "5",
    "--rounds",
    "2",
    "--processes",
    "1",
    "2",
    "--seconds",
    "0.2",
    "--savestate-repeats",
    "3",
    "--frameskip",
    "1",
    "4",
]


@pytest.fixture
def content(testcore_data: Path) -> Path:
    return testcore_data.parent / "roms" / "sub" / "renamed.gthc"


@pytest.fixture
def data_dir(testcore_path: Path, testcore_data: Path) -> Path:
    """testcore_data with the test core where `gth bench` looks for a core."""
    path = core_library(testcore_data, "gth_testcore")
    path.parent.mkdir()
    shutil.copy(testcore_path, path)
    return testcore_data


def test_gth_bench_measures_and_replays_the_test_core(
    data_dir: Path, content: Path, tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    out, report = tmp_path / "out", tmp_path / "bench.json"
    argv = ["bench", str(data_dir), "--content", f"gth_testcore={content}", "--out", str(out)]
    argv += [*TINY, "--determinism-frames", "120", "--json", str(report)]
    assert cli.main(argv) == 0

    run = json.loads(report.read_text(encoding="utf-8"))
    assert run["machine"]["cpu_count"] >= 1 and run["settings"]["processes"] == [1, 2]
    (target,) = run["targets"]
    assert target["target"] == "renamed" and target["core"] == "gth_testcore"
    assert not target["pinned"]  # the lock pins no test core
    assert set(target["speed"]) == {"1", "4"}
    for result in target["speed"].values():
        assert result["fps"] > 0 and result["native_step_us"] > 0 and result["call_us"] >= 0
    for frameskip in ("1", "4"):
        runs = target["parallel"][frameskip]
        assert [r["processes"] for r in runs] == [1, 2]
        assert all(len(r["per_process_fps"]) == r["processes"] and r["fps"] > 0 for r in runs)
    state = target["savestate"]
    assert state["size"] > 0 and state["reload_first_difference"] is None
    # The reset frames' step (frameskip 4), then 30 steps of 4 frames.
    result = target["determinism"]
    assert result["frames"] == result["replayed"] == 124 and result["desync"] is None
    assert Path(result["recording"]).parent == out / "renamed"

    tables = capsys.readouterr().out
    assert "| renamed | `gth_testcore` |" in tables and "| renamed | 124 | 124 | none |" in tables


def test_replay_check_reports_the_desynced_frame(
    data_dir: Path,
    content: Path,
    tmp_path: Path,
    zstd_raw_frame: Callable[[bytes], bytes],
):
    target = bench.content_target("gth_testcore", content, data_dir)
    result = bench.determinism(target, 40, tmp_path, seed=3)
    assert result["desync"] is None
    # Rewrite inputs.bin with A toggled on one frame: the masks are the reset step's
    # no-op frames, then the seeded random actions' (as _record_worker plays them).
    table = target.config().joypad_table()
    actions = np.random.default_rng(3).integers(0, target.config().n_actions, 10)
    masks = [0] + [table[int(action)] for action in actions]
    inputs = bytearray(b"GTHINPUT")
    for mask in masks:
        for i in range(4):
            inputs += struct.pack("<HB", mask, int(i == 3))
    tampered = 17
    at = 8 + 3 * (tampered - 1)
    inputs[at : at + 2] = struct.pack("<H", struct.unpack_from("<H", inputs, at)[0] ^ 1 << 8)
    recording = Path(result["recording"])
    (recording / "inputs.bin").write_bytes(zstd_raw_frame(bytes(inputs)))

    checked = bench.replay_check(target, recording)
    assert checked["desync"]["frame"] == tampered
    assert f"frame {tampered}" in checked["desync"]["error"]


def test_game_targets_name_the_content_as_the_game_does(data_dir: Path, tmp_path: Path):
    """A game's target keeps the game's own [content] table, so recordings identify the
    content as the game's do, and a clone inside its parent's archive (in_parent), whose path
    doesn't exist, can be benchmarked (docs/performance.md)."""
    game = _native.GameConfig.load(GAMES_DIR / "testcore" / "game.toml")
    target = bench.game_target("testcore", data_dir, None)  # GTH_ROM_DIR, from testcore_data
    assert target.content_path.name == "renamed.gthc"  # found by SHA-1
    assert target.config().content_toml == game.content_toml
    result = bench.determinism(target, 40, tmp_path / "out", seed=0)
    assert result["desync"] is None and result["replayed"] == result["frames"]
    meta = tomllib.loads((Path(result["recording"]) / "meta.toml").read_text(encoding="utf-8"))
    sha1 = hashlib.sha1(target.content_path.read_bytes()).hexdigest()
    assert meta["content"] == {"file": "renamed.gthc", "sha1": sha1}

    # A merged set: the clone has no file of its own, only a place in its parent's archive.
    games = tmp_path / "games"
    (games / "merged").mkdir(parents=True)
    testcore_toml = (GAMES_DIR / "testcore" / "game.toml").read_text(encoding="utf-8")
    before, rest = testcore_toml.split("[content]")
    clone = (
        '[content]\nfile = "gth_testcore_clone.zip"\nin_parent = true\n'
        'members = [{ name = "clone.bin", size = 1, sha1 = "'
        + "0" * 40
        + '", crc32 = "00000000" }]\nparents = ['
    )
    (games / "merged" / "game.toml").write_text(
        before + clone + rest.split("parents = [", 1)[1], encoding="utf-8"
    )
    merged = _native.GameConfig.load(games / "merged" / "game.toml")
    target = bench.game_target("merged", data_dir, None, games)
    assert target.content_path.name == "gth_testcore_clone.zip"
    assert not target.content_path.exists()
    config = target.config()
    assert config.content_in_parent and config.content_toml == merged.content_toml


def test_default_processes_fit_the_machine(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(bench.os, "cpu_count", lambda: 10)
    assert bench.default_processes() == [1, 2, 4, 9]
    monkeypatch.setattr(bench.os, "cpu_count", lambda: 2)
    assert bench.default_processes() == [1, 2]
    monkeypatch.setattr(bench.os, "cpu_count", lambda: None)
    assert bench.default_processes() == [1]


def test_gth_bench_usage_errors(data_dir: Path, content: Path, tmp_path: Path):
    out = ["--out", str(tmp_path)]
    assert cli.main(["bench", str(data_dir), *out]) == 2
    assert cli.main(["bench", str(data_dir), "--content", "no-equals", *out]) == 2
    assert cli.main(["bench", str(data_dir), "--content", f"fceumm={tmp_path}/none", *out]) == 2
    # A core that isn't fetched is a config error, not a failed measurement.
    assert cli.main(["bench", str(data_dir), "--content", f"fceumm={content}", *out]) == 2
    target = ["bench", str(data_dir), "--content", f"gth_testcore={content}", *out]
    for bad in [
        ["--steps", "0"],
        ["--rounds", "0"],
        ["--savestate-repeats", "0"],
        ["--seconds", "0"],
        ["--obs-size", "0", "84"],
        ["--determinism-frames", "2"],
        ["--determinism-frames", "122"],
    ]:
        assert cli.main([*target, *bad]) == 2, bad


def test_bench_real_rom(rom: Callable[[str], Path], core: Callable[..., Path], tmp_path: Path):
    """A short `gth bench --game gunhed` on its pinned core and the user's ROM: skips without
    them (docs/testing.md). Pins nothing about speed; checks the run replays."""
    content, library = rom("gunhed"), core("gunhed")
    report = tmp_path / "bench.json"
    argv = ["bench", str(library.parent.parent), "--game", "gunhed", "--out", str(tmp_path)]
    argv += ["--rom-dir", str(content.parent), *TINY, "--determinism-frames", "600"]
    assert cli.main([*argv, "--json", str(report)]) == 0

    (target,) = json.loads(report.read_text(encoding="utf-8"))["targets"]
    assert target["core"] == "mednafen_pce_fast" and target["pinned"]
    assert target["savestate"]["reload_first_difference"] is None
    result = target["determinism"]
    assert result["frames"] == result["replayed"] == 604 and result["desync"] is None
