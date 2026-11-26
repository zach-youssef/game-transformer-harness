"""Shared fixtures: the test core (docs/libretro.md, "Loading the test core in tests")."""

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

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
