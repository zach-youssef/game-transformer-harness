import re
import tomllib
from pathlib import Path

import gtharness
from gtharness import _native

ROOT = Path(__file__).resolve().parents[1]


def test_version_is_the_workspace_version():
    cargo = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    pyproject = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    assert _native.__version__ == cargo["workspace"]["package"]["version"]
    assert _native.__version__ == pyproject["project"]["version"]
    assert gtharness.__version__ == _native.__version__


def test_build_info_fields():
    info = _native.build_info()
    keys = ["git_dirty", "git_sha", "profile", "rustc", "target", "version"]
    assert list(info) == keys  # sorted, so serialized metadata is stable
    assert info["version"] == _native.__version__
    assert re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}|unknown", info["git_sha"])
    assert info["git_dirty"] in {"true", "false", "unknown"}
    assert info["target"].count("-") >= 2  # a target triple, e.g. aarch64-apple-darwin
    assert info["profile"] in {"debug", "release"}
    assert info["rustc"].startswith("rustc ")
