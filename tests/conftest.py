"""Shared fixtures: the test core (docs/libretro.md, "Loading the test core in tests"), the
`rom` lookup every ROM test uses (docs/roms.md), the `core` lookup of tests that run a real
core (docs/testing.md, "ROM tests") and the test core's game files."""

import json
import os
import struct
import subprocess
import sys
import zipfile
from collections.abc import Callable
from pathlib import Path

import pytest

from gtharness import _native, cores
from gtharness.env import GAMES_DIR, core_library

ROOT = Path(__file__).resolve().parent.parent
DATA_DIR = ROOT / "data"
"""The checkout's git-ignored data directory, where `gth cores fetch data` puts the cores."""
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


@pytest.fixture
def core() -> Callable[..., Path]:
    """``core(game_id, data_dir=DATA_DIR)``: the game's core in ``data_dir/cores/``. Skips the
    test when it is missing or isn't the build `cores.lock` pins for this platform (a ROM test
    pins values that another build may not reproduce), so a machine with ROMs but no fetched
    core skips rather than fails."""

    def find(game_id: str, data_dir: Path = DATA_DIR) -> Path:
        name = _native.GameConfig.load(GAMES_DIR / game_id / "game.toml").core
        path = core_library(data_dir, name)
        if not path.is_file():
            pytest.skip(
                f"{game_id}: no {path.name} in {path.parent} (gth cores fetch, docs/cores.md)"
            )
        lock = _native.CoresLock.load(cores.LOCK)
        if cores.TARGET not in lock.artifacts(name):
            pytest.skip(
                f"{game_id}: cores.lock pins no {cores.TARGET} build of {name} yet: its release"
                " isn't written into the lock (docs/cores.md)"
            )
        if not lock.pins(name, cores.TARGET, path):
            pytest.skip(f"{game_id}: {path} isn't the build cores.lock pins (gth cores fetch)")
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


@pytest.fixture
def zstd_raw_frame() -> Callable[[bytes], bytes]:
    """``zstd_raw_frame(data)``: ``data`` as a zstd frame, for tests that rewrite a
    recording's files (Python 3.12 has no zstd module)."""
    return _zstd_raw_frame


def _zstd_raw_frame(data: bytes) -> bytes:
    """``data`` as a zstd frame of raw (uncompressed) blocks (RFC 8878): single segment, a
    4-byte content size, no checksum."""
    frame = struct.pack("<IBI", 0xFD2FB528, 0b1010_0000, len(data))
    blocks = [data[i : i + 128 * 1024] for i in range(0, len(data), 128 * 1024)] or [b""]
    for i, block in enumerate(blocks):
        last = int(i == len(blocks) - 1)
        frame += (len(block) << 3 | last).to_bytes(3, "little") + block
    return frame
