"""The ALE core (docs/cores.md, "Forks") loads in the harness and refuses what isn't a ROM it
supports cleanly: no content, a path that doesn't exist and a file whose MD5 is no game's all
fail the load with CoreLoadError, and the process goes on (ALE's own loadROM would exit it).
Needs no ROM. Uses data/cores/ale_libretro.<ext> (`gth cores fetch data`, or a local build
from scripts/build-core.sh copied there) and skips without it, as on CI, which fetches no
cores; any build will do, pinned or not, since nothing here depends on its emulation."""

from pathlib import Path

import pytest

from gtharness import _native
from gtharness.env import core_library
from gtharness.errors import CoreLoadError

DATA_DIR = Path(__file__).resolve().parent.parent / "data"


@pytest.fixture
def ale_core() -> Path:
    path = core_library(DATA_DIR, "ale")
    if not path.is_file():
        pytest.skip(f"no {path.name} in {path.parent} (gth cores fetch, docs/cores.md)")
    return path


def load(core_path: Path, content: Path | None, options: dict[str, str]) -> _native.Core:
    return _native.Core(
        core_path, content, core_path.parent, options, 0, 84, 84, None, 0, "area", False, True
    )


def test_reports_ale(ale_core: Path) -> None:
    info = _native.probe_core(ale_core, ale_core.parent)
    assert info["library_name"] == "ALE"
    assert info["valid_extensions"] == "bin|a26"
    version, commit = info["library_version"].split(" ")
    assert version.count(".") == 2
    assert len(commit) == 7


def test_refuses_what_isnt_a_supported_rom(ale_core: Path, tmp_path: Path) -> None:
    unknown = tmp_path / "unknown.bin"
    unknown.write_bytes(bytes(range(256)) * 16)  # 4 KiB, a 2600 cartridge's size
    named = tmp_path / "breakout.bin"  # a supported game's name with the wrong bytes
    named.write_bytes(bytes(4096))
    for content in (None, tmp_path / "missing" / "pong.bin", unknown, named):
        for options in ({}, {"ale_mode": "default", "ale_difficulty": "0", "ale_gray": "on"}):
            with pytest.raises(CoreLoadError, match="refused the content"):
                load(ale_core, content, options)
    # The core is still usable after the refusals.
    assert _native.probe_core(ale_core, ale_core.parent)["library_name"] == "ALE"
