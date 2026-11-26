"""Shared fixtures: the test core (docs/libretro.md, "Loading the test core in tests"), the
`rom` lookup every ROM test uses (docs/roms.md) and the test core's game files."""

import json
import os
import subprocess
import sys
import zipfile
from collections.abc import Callable
from pathlib import Path

import pytest

from gtharness import _native
from gtharness.env import GAMES_DIR

ROOT = Path(__file__).resolve().parent.parent
LIBRARY = {"win32": "gth_testcore.dll", "darwin": "libgth_testcore.dylib"}.get(
    sys.platform, "libgth_testcore.so"
)


@pytest.fixture(scope="session")
def testcore_path() -> Path:
    """The test core's library, built once per session before any test loads it. The same
    rule as gth_test_support::testcore_path: build with cargo and take the path from cargo's
    JSON messages, which works with any CARGO_TARGET_DIR. Cargo runs directly (not nested in
    `cargo run`), so no outer cargo holds the build lock on Windows."""
    cargo = os.environ.get("CARGO", "cargo")
    result = subprocess.run(
        [cargo, "build", "--locked", "-p", "gth-testcore", "--message-format=json"],
        cwd=ROOT,
        capture_output=True,
        encoding="utf-8",
        check=False,
    )
    assert result.returncode == 0, result.stderr
    for line in result.stdout.splitlines():
        message = json.loads(line) if line.startswith("{") else {}
        if message.get("reason") == "compiler-artifact":
            for file in message.get("filenames", []):
                if Path(file).name == LIBRARY:
                    return Path(file)
    raise AssertionError(f"cargo didn't report {LIBRARY}")


@pytest.fixture
def rom() -> Callable[[str], Path]:
    """``rom(game_id)``: the game's content file in the ROM directory (GTH_ROM_DIR or the user
    config's rom_dir), found by SHA-1. Skips the test when the directory is unset or the file
    isn't in it, so tests that need a user's ROM pass on CI, which has none."""

    def find(game_id: str) -> Path:
        config = _native.GameConfig.load(GAMES_DIR / game_id / "game.toml")
        rom_dir = _native.rom_dir()
        if rom_dir is None:
            pytest.skip(f"{game_id}: no ROM directory (set GTH_ROM_DIR, docs/roms.md)")
        path = config.find_content(rom_dir)
        if path is None:
            pytest.skip(f"{game_id}: {config.content_file} isn't in {rom_dir}")
        return path

    return find


# games/testcore/game.toml's files: not ROMs, so tests write them.
TESTCORE_CONTENT = b"gth-testcore content: seeds the test core's PRNG\n"
TESTCORE_BIOS = b"gth-testcore stand-in BIOS: the test core reads none\n"


@pytest.fixture
def testcore_data(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A data directory with the test core game's BIOS files, and GTH_ROM_DIR pointing at a
    ROM directory with its content (renamed, so it is found by SHA-1) and parent set."""
    roms = tmp_path / "roms" / "sub"
    roms.mkdir(parents=True)
    (roms / "renamed.gthc").write_bytes(TESTCORE_CONTENT)
    with zipfile.ZipFile(roms / "gth_testcore_parent.zip", "w", zipfile.ZIP_DEFLATED) as parent:
        parent.writestr("parent.bin", TESTCORE_BIOS)
    monkeypatch.setenv("GTH_ROM_DIR", str(tmp_path / "roms"))
    system = tmp_path / "data" / "system"
    (system / "gth").mkdir(parents=True)
    (system / "gth_testcore_bios.bin").write_bytes(TESTCORE_BIOS)
    with zipfile.ZipFile(system / "gth" / "gth_testcore_bios.zip", "w") as bios:
        bios.writestr("bios.bin", TESTCORE_BIOS)
    return tmp_path / "data"
