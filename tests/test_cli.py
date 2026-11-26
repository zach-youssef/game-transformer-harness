"""``gth audit`` (docs/roms.md, "Auditing") on the test core game's files, which conftest
writes, and on fabricated games."""

import json
import shutil
import zipfile
from pathlib import Path

import pytest

from gtharness.cli import main
from gtharness.env import GAMES_DIR

_SHA1 = "1a03ac5869e1d56cfd138db9790087dd926c18b3"  # games/testcore content


def run(capsys: pytest.CaptureFixture[str], *args: str) -> tuple[int, str]:
    code = main(["audit", *args])
    return code, capsys.readouterr().out


def test_the_testcore_game_audits_ok(testcore_data: Path, capsys: pytest.CaptureFixture[str]):
    code, out = run(capsys, str(testcore_data), "--game", "testcore")
    assert code == 0, out
    assert "testcore: ok" in out
    code, out = run(capsys, str(testcore_data), "--game", "testcore", "--json")
    report = json.loads(out)
    files = report["games"]["testcore"]["files"]
    assert [(file["role"], file["status"]) for file in files] == [
        ("content", "ok"),
        ("parent", "ok"),
        ("bios", "ok"),
        ("bios", "ok"),
    ]
    assert files[0]["path"].endswith("renamed.gthc"), "found by SHA-1"
    assert report["system_dir"] == str(testcore_data / "system")


def test_bad_members_fail_the_audit(testcore_data: Path, capsys: pytest.CaptureFixture[str]):
    bios = testcore_data / "system" / "gth" / "gth_testcore_bios.zip"
    with zipfile.ZipFile(bios, "w") as archive:
        archive.writestr("bios.bin", b"not the BIOS")
        archive.writestr("readme.txt", b"extra")
    code, out = run(capsys, str(testcore_data), "--game", "testcore")
    assert code == 1
    assert "testcore: bad-hash" in out
    assert "bad-hash   bios.bin found size 12" in out and "1 other members\n" in out + "\n"
    with zipfile.ZipFile(bios, "a", compression=zipfile.ZIP_BZIP2) as archive:
        archive.writestr("packed.bin", b"bzip2 is unsupported")
    out = run(capsys, str(testcore_data), "--game", "testcore")[1]
    assert "2 other members (1 unreadable)" in out
    report = json.loads(run(capsys, str(testcore_data), "--game", "testcore", "--json")[1])
    member = report["games"]["testcore"]["files"][3]["members"][0]
    assert member["status"] == "bad-hash" and member["found"]["size"] == 12


def test_all_games_but_the_test_cores_by_default(
    testcore_data: Path, tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    games = tmp_path / "games"
    shutil.copytree(GAMES_DIR / "testcore", games / "testcore")
    (games / "absent").mkdir()
    absent = (GAMES_DIR / "testcore" / "game.toml").read_text(encoding="utf-8")
    absent = absent.replace('core = "gth_testcore"', 'core = "other"')
    (games / "absent" / "game.toml").write_text(absent.replace(_SHA1, "0" * 40), "utf-8")
    code, out = run(capsys, str(testcore_data), "--games-dir", str(games), "--json")
    report = json.loads(out)
    assert code == 1 and list(report["games"]) == ["absent"]
    assert report["games"]["absent"]["files"][0]["status"] == "missing"


def test_a_missing_rom_dir_is_a_usage_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
):
    monkeypatch.delenv("GTH_ROM_DIR", raising=False)
    monkeypatch.setenv("GTH_CONFIG", str(tmp_path / "none.toml"))
    assert main(["audit", str(tmp_path)]) == 2
    assert "no ROM directory" in capsys.readouterr().err
    assert main(["audit", str(tmp_path), "--rom-dir", str(tmp_path / "nope")]) == 2
    assert "doesn't exist" in capsys.readouterr().err


def test_no_games_is_a_usage_error(
    testcore_data: Path, tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    games = tmp_path / "games"
    games.mkdir()
    args = ["audit", str(testcore_data), "--games-dir", str(games)]
    assert main(args) == 2
    assert "no games to audit" in capsys.readouterr().err
    shutil.copytree(GAMES_DIR / "testcore", games / "testcore")
    assert main(args) == 2, "the test core's game is skipped"
    assert "no games to audit" in capsys.readouterr().err
