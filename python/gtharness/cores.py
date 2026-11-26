"""``gth cores``: the pinned core builds in ``cores.lock`` (docs/cores.md).

``fetch`` downloads this platform's builds from the lock's GitHub release (or copies them from
a directory), checks each one's SHA-256 against the lock, loads it without a game and checks
the ``library_version`` it reports. ``plan``, ``check`` and ``release`` are the cores
workflow's steps; ``plan`` also checks the patches the lock applies to a core's source. The
hashing, naming and loading are Rust (``_native``); this module moves files.
"""

from __future__ import annotations

import os
import shutil
import tempfile
import urllib.request
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Any

from gtharness import _native
from gtharness.errors import ConfigError, HarnessError

LOCK = Path(__file__).resolve().parents[2] / "cores.lock"
"""The checkout's ``cores.lock``."""

RELEASES = "https://github.com/zach-youssef/game-transformer-harness/releases/download"
"""Where the cores workflow publishes each lock's builds, under the lock's release tag."""

TARGET = _native.build_info()["target"]
"""This platform's target triple, the key of its builds in ``cores.lock``."""

Report = Callable[[str], None]


def plan(lock: _native.CoresLock, lock_dir: Path) -> dict[str, Any]:
    """What the cores workflow builds: the release tag and each core's source, with its
    patches as paths relative to ``lock_dir`` (the lock's directory) in the order they apply.
    Raises ConfigError unless each patch there has the SHA-256 the lock pins."""
    cores = []
    for name in lock.names:
        lock.check_patches(name, lock_dir)
        cores.append({"name": name, **lock.core(name), "patches": lock.patches(name)})
    return {"tag": lock.release_tag, "cores": cores}


def fetch(lock: _native.CoresLock, data_dir: Path, source: str | None, report: Report) -> list[str]:
    """Puts this platform's build of every core in ``data_dir/cores`` and checks it: its
    SHA-256 against the lock, then the ``library_version`` it reports when loaded without a
    game. ``source`` is a directory or URL holding the release assets; None is the lock's
    GitHub release. A file already there with the pinned hash isn't fetched again. Returns the
    cores that failed a check, after reporting why; a build that fails its hash is never put
    in place. Raises ConfigError if the lock has no build of a core for this platform."""
    if source is None:
        source = f"{RELEASES}/{lock.release_tag}"
    unpinned = [name for name in lock.names if TARGET not in lock.artifacts(name)]
    if unpinned:
        raise ConfigError(
            f"cores.lock has no {TARGET} build of {', '.join(unpinned)}: the cores release for"
            " this lock may not have been written into it yet (docs/cores.md)"
        )
    cores_dir = data_dir / "cores"
    cores_dir.mkdir(parents=True, exist_ok=True)
    failed = []
    for name in lock.names:
        path = cores_dir / _native.library_file(name, TARGET)
        try:
            if lock.pins(name, TARGET, path):
                report(f"{name}: {path} has the pinned hash")
            else:
                asset = _native.asset_name(name, TARGET)
                partial = path.with_name(path.name + ".part")
                try:
                    _copy(source, asset, partial)
                    lock.check_artifact(name, TARGET, partial)
                    os.replace(partial, path)
                finally:
                    partial.unlink(missing_ok=True)
                report(f"{name}: fetched {asset} into {path}")
            version = _probe(lock, name, path, data_dir / "system")
            report(f"{name}: loads and reports {version!r}")
        except (HarnessError, OSError) as error:
            report(f"{name}: FAILED: {error}")
            failed.append(name)
    return failed


def check(lock: _native.CoresLock, cores_dir: Path, report: Report) -> list[str]:
    """Loads each core's library in ``cores_dir`` (``<name>_libretro.<ext>``) without a game
    and checks the ``library_version`` it reports, as the cores workflow does for fresh builds
    on each platform. Their hashes are reported but not required to match: a build made for a
    new pin has none in the lock until its release. Each core gets an empty system (BIOS)
    directory. Returns the cores that failed."""
    with tempfile.TemporaryDirectory(prefix="gth-cores-check-") as system_dir:
        return _check(lock, cores_dir, Path(system_dir), report)


def _check(lock: _native.CoresLock, cores_dir: Path, system_dir: Path, report: Report) -> list[str]:
    failed = []
    for name in lock.names:
        path = cores_dir / _native.library_file(name, TARGET)
        try:
            version = _probe(lock, name, path, system_dir)
        except (HarnessError, OSError) as error:
            report(f"{name}: FAILED: {error}")
            failed.append(name)
            continue
        if TARGET not in lock.artifacts(name):
            pin = f"no {TARGET} hash pinned"
        else:
            pin = "the pinned hash" if lock.pins(name, TARGET, path) else "not the pinned hash"
        report(f"{name}: loads and reports {version!r}; {pin}")
    return failed


def release(
    lock: _native.CoresLock, builds: Path, out: Path, targets: Sequence[str]
) -> _native.CoresLock:
    """Lays out a release in ``out`` from the cores workflow's builds in
    ``builds/<target>/<library file>``: each build under its asset name, the lock with their
    hashes written in (``cores.lock``) and ``SHA256SUMS``. Returns that lock."""
    filled = lock.with_builds(builds, list(targets))
    out.mkdir(parents=True, exist_ok=True)
    sums = []
    for name in filled.names:
        for target in targets:
            asset = _native.asset_name(name, target)
            shutil.copyfile(builds / target / _native.library_file(name, target), out / asset)
            sums.append(f"{filled.artifacts(name)[target]}  {asset}\n")
    (out / "cores.lock").write_text(filled.to_toml(), encoding="utf-8")
    (out / "SHA256SUMS").write_text("".join(sums), encoding="utf-8")
    return filled


def pinned_lock(lock_path: Path, name: str, core_path: Path) -> _native.CoresLock | None:
    """The lock at ``lock_path`` if it pins ``core_path`` as this platform's build of core
    ``name`` (by SHA-256), else None: what a recording keeps as its core's lock entry, which
    then says exactly which build made it. None, too, when there is no lock there."""
    if not lock_path.is_file():
        return None
    lock = _native.CoresLock.load(lock_path)
    return lock if lock.pins(name, TARGET, core_path) else None


def _probe(lock: _native.CoresLock, name: str, path: Path, system_dir: Path) -> str:
    """Loads the core without a game and checks its version; returns the version."""
    version = _native.probe_core(path, system_dir)["library_version"]
    lock.check_library_version(name, version)
    return version


def _copy(source: str, asset: str, dest: Path) -> None:
    """Copies ``asset`` from ``source`` (a directory, or a URL that ``asset`` is appended
    to) to ``dest``."""
    if "://" not in source:
        shutil.copyfile(Path(source) / asset, dest)
        return
    url = f"{source.rstrip('/')}/{asset}"
    with urllib.request.urlopen(url, timeout=60) as response, dest.open("wb") as file:
        shutil.copyfileobj(response, file)
