"""RAM search (docs/games.md, "RAM search") through `_native.RamSearch`, `gtharness.ramsearch`
and ``gth ram-search``, on the test core, whose score is BCD at system RAM offset 0."""

import json
from collections.abc import Iterator
from pathlib import Path

import pytest

from gtharness import _native
from gtharness.cli import main
from gtharness.env import GAMES_DIR, RetroEnv
from gtharness.errors import HarnessError
from gtharness.ramsearch import assert_ram, ram_table
from gtharness.rewards import score_delta

A = 1 << 8  # RETRO_DEVICE_ID_JOYPAD_A


@pytest.fixture
def core(testcore_path: Path, tmp_path: Path) -> Iterator[_native.Core]:
    core = _native.Core(testcore_path, None, tmp_path, {}, 0, 16, 16, None, 0, "area")
    try:
        yield core
    finally:
        core.close()


def test_a_search_narrows_to_the_score(core: _native.Core):
    search = _native.RamSearch(core, "system_ram", ["bcd4", "le2"])
    assert (search.space, search.views) == ("system_ram", ["bcd4", "le2"])
    assert len(search) == 253 + 255
    core.run_inputs([(A, 12)])
    # Score bytes 00 00 00 12: bcd4 at offsets 0-3 and le2 at 2-3 read them.
    assert search.filter(core, "increased", None) == 6
    core.run_inputs([(0, 5)])
    assert search.filter(core, "equal", None) == 6
    assert search.filter(core, "equals", 12) == 1
    assert search.candidates(10) == [
        {
            "view": "bcd4",
            "memory": "system_ram",
            "offset": 0,
            "size": 4,
            "encoding": "bcd",
            "swap16": False,
            "value": 12,
        }
    ]
    assert search.watch(core, [(A, 2), (0, 1)], 5) == [[13], [14], [14]]
    assert search.candidates(1)[0]["value"] == 12, "watch takes no snapshot"
    core.run_inputs([(A, 1)])
    assert search.rebase(core) == 1
    assert search.candidates(1)[0]["value"] == 15


def test_memory_map_candidates_have_addresses(core: _native.Core):
    search = _native.RamSearch(core, "memory_map", ["be2-swap16"])
    assert len(search) == 128, "even addresses only"
    assert search.candidates(1)[0]["address"] == 0
    assert search.candidates(0) == []


def test_bad_arguments_raise(core: _native.Core):
    with pytest.raises(ValueError, match="space"):
        _native.RamSearch(core, "work_ram", ["le1"])
    with pytest.raises(ValueError, match="view"):
        _native.RamSearch(core, "system_ram", ["le3"])
    with pytest.raises(HarnessError, match="views"):
        _native.RamSearch(core, "system_ram", [])
    with pytest.raises(HarnessError, match="no save_ram"):
        _native.RamSearch(core, "save_ram", ["le1"])
    search = _native.RamSearch(core, "system_ram", ["le1"])
    with pytest.raises(ValueError, match="value"):
        search.filter(core, "equals", None)
    with pytest.raises(ValueError, match="value"):
        search.filter(core, "equal", 3)
    assert sorted(_native.ram_search_views()) == sorted(
        [
            "le1",
            "bcd1",
            "le2",
            "be2",
            "bcd2",
            "bcd2-swap16",
            "le4",
            "be4",
            "bcd4",
            "le4-swap16",
            "be4-swap16",
            "bcd4-swap16",
        ]
    )


@pytest.fixture
def ram_search(testcore_path: Path, testcore_data: Path) -> list[str]:
    """``gth ram-search`` on the test core game, up to the script argument."""
    return ["ram-search", "testcore", str(testcore_data), "--core-path", str(testcore_path)]


SCRIPT = """
space = "{space}"
views = ["all"]

[[step]]
press = ["a"]
frames = 120
expect = "increased"

[[step]]
frames = 30
expect = "equal"

[[step]]
expect = "equals"
value = 123
"""


@pytest.mark.parametrize("space", ["system_ram", "memory_map"])
def test_gth_ram_search_finds_the_bcd_score(
    space: str, ram_search: list[str], tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    script = tmp_path / "score.toml"
    script.write_text(SCRIPT.format(space=space), encoding="utf-8")
    # The game's reset leaves score 3; 120 frames of shot make it 123.
    code = main([*ram_search, str(script), "--json"])
    result = json.loads(capsys.readouterr().out)
    assert code == 0
    counts = result["counts"]
    assert counts == sorted(counts, reverse=True) and counts[-1] == 2
    where = ["offset", "memory"] if space == "system_ram" else ["address"]
    found = [(c["view"], *(c[key] for key in where), c["value"]) for c in result["candidates"]]
    expected = [("bcd2", 2, 123), ("bcd4", 0, 123)]
    if space == "system_ram":
        expected = [(view, at, "system_ram", value) for view, at, value in expected]
    assert found == expected

    code = main([*ram_search, str(script), "--watch", "2"])
    out = capsys.readouterr().out
    assert code == 0
    counts_line = " -> ".join(map(str, counts))
    assert out.startswith(f"{space}, views le1, bcd1, ") and f": {counts_line} candidates\n" in out
    assert "# bcd4, value 123\n[ram.candidate_1]\n" in out
    assert out.endswith("    1     123     123\n    2     123     123\n")


def test_gth_ram_search_reports_errors(
    ram_search: list[str], tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    script = tmp_path / "lives.toml"
    header = 'space = "system_ram"\nviews = ["le1"]\n[[step]]\n'
    script.write_text(header + 'state = "die.state"\n', encoding="utf-8")
    assert main([*ram_search, str(script)]) == 2
    assert "die.state" in capsys.readouterr().err
    script.write_text(header + 'expect = "equals"\nvalue = 300\n', encoding="utf-8")
    assert main([*ram_search, str(script)]) == 1
    assert ": 256 -> 0 candidates" in capsys.readouterr().out
    script.write_text(header + "frames = 1\n", encoding="utf-8")
    assert main([*ram_search, str(script), "--watch", "200000"]) == 2
    assert "watch at most 108000 frames" in capsys.readouterr().err
    assert main([*ram_search, str(script), "--limit", "300", "--watch", "100000"]) == 2
    assert "watch at most 10000000 values" in capsys.readouterr().err
    for bad in (["--limit", "-1"], ["--watch", "x"], ["--watch", str(1 << 32)]):
        with pytest.raises(SystemExit) as exited:
            main([*ram_search, str(script), *bad])
        assert exited.value.code == 2
        assert bad[1] in capsys.readouterr().err


def test_ram_table_is_a_game_toml_variable():
    candidate = {"memory": "system_ram", "offset": 16, "size": 2, "encoding": "be"}
    lives = ram_table("lives", {**candidate, "swap16": False})
    assert lives == '[ram.lives]\nmemory = "system_ram"\noffset = 0x10\nsize = 2\nencoding = "be"'
    mapped = {"address": 0xFF8000, "size": 4, "encoding": "bcd", "swap16": True}
    hiscore = ram_table("hiscore", mapped)
    assert hiscore == (
        '[ram.hiscore]\naddress = 0xff8000\nsize = 4\nencoding = "bcd"\nswap16 = true'
    )
    game = (GAMES_DIR / "testcore" / "game.toml").read_text(encoding="utf-8")
    config = _native.GameConfig.parse(f"{game}\n{lives}\n{hiscore}\n", "game.toml")
    assert {"lives", "hiscore"} <= set(config.ram_names)


def test_assert_ram_checks_a_game_s_variables(testcore_path: Path, testcore_data: Path):
    env = RetroEnv.from_game(
        "testcore",
        testcore_data,
        frameskip=1,
        obs_size=(16, 16),
        action_format="flat",
        reward=score_delta,
        sticky_prob=0.0,
        max_episode_frames=None,
        noop_max=0,
        core_path=testcore_path,
    )
    try:
        _, info = env.reset()
        assert_ram(info["ram"], "score", 3)
        assert_ram(info["ram"], "score_tens", lambda tens: tens == 30)
        with pytest.raises(AssertionError, match="score is 3, expected 4"):
            assert_ram(info["ram"], "score", 4)
        with pytest.raises(AssertionError, match="score is 3, which fails"):
            assert_ram(info["ram"], "score", lambda score: score > 3)
        with pytest.raises(AssertionError, match="no RAM variable 'lives'"):
            assert_ram(info["ram"], "lives", 3)
    finally:
        env.close()


def test_playing_waits_for_the_recording(core: _native.Core, tmp_path: Path):
    # watch and scripts run frames (and load states) outside the recorder; filters only read.
    # The fixture's core loads states as they are, so the recording's game must too.
    text = (GAMES_DIR / "testcore" / "game.toml").read_text(encoding="utf-8")
    game = _native.GameConfig.parse(text.replace("settle_frames = 1", "settle_frames = 0"), "t")
    assert game.settle_frames == 0
    search = _native.RamSearch(core, "system_ram", ["bcd4"])
    script = tmp_path / "s.toml"
    script.write_text('space = "system_ram"\nviews = ["le1"]\n[[step]]\n', encoding="utf-8")
    core.start_recording(tmp_path / "a.gthrec", game, "testcore", None, "policy", "p", "l", 60)
    with pytest.raises(ValueError, match="finish the recording first"):
        search.watch(core, [(A, 1)], 1)
    with pytest.raises(ValueError, match="finish the recording first"):
        _native.RamSearch.run_script(core, script)
    core.step(3, A, 0.0)
    assert search.filter(core, "increased", None) > 0
    assert core.finish_recording() == tmp_path / "a.gthrec"
