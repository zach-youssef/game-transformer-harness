"""``gth cores`` (docs/cores.md) with the test core standing in for a pinned core: a lock
fabricated around its build, a release laid out from it, and fetches from that directory."""

import json
import shutil
import tomllib
from pathlib import Path
from typing import Any

import pytest

from gtharness import _native, cores
from gtharness.cli import main
from gtharness.env import GAMES_DIR
from gtharness.errors import ConfigError

NAME = "gth_testcore"
COMMIT = "0123456789abcdef0123456789abcdef01234567"


def lock_text(version: str, artifacts: dict[str, str] | None = None) -> str:
    lines = [
        f"[cores.{NAME}]",
        'repo = "https://example.invalid/gth-testcore"',
        f'commit = "{COMMIT}"',
        f'library_version = "{version}"',
        "",
        f"[cores.{NAME}.artifacts]",
        *(f'"{target}" = "{sha}"' for target, sha in (artifacts or {}).items()),
    ]
    return "\n".join(lines) + "\n"


@pytest.fixture
def version(testcore_path: Path) -> str:
    return _native.probe_core(testcore_path, testcore_path.parent)["library_version"]


@pytest.fixture
def builds(tmp_path: Path, testcore_path: Path) -> Path:
    """The cores workflow's builds directory with the test core built for this platform."""
    target_dir = tmp_path / "builds" / cores.TARGET
    target_dir.mkdir(parents=True)
    shutil.copyfile(testcore_path, target_dir / _native.library_file(NAME, cores.TARGET))
    return tmp_path / "builds"


@pytest.fixture
def released(tmp_path: Path, builds: Path, version: str) -> tuple[Path, Path]:
    """A release laid out with ``gth cores release``, and the lock with its hashes."""
    unpinned = tmp_path / "unpinned.lock"
    unpinned.write_text(lock_text(version), encoding="utf-8")
    out = tmp_path / "release"
    args = ["cores", "--lock", str(unpinned), "release", str(builds), "--out", str(out)]
    assert main([*args, "--target", cores.TARGET]) == 0
    return out, out / "cores.lock"


def run(capsys: pytest.CaptureFixture[str], lock: Path, *args: str) -> tuple[int, str]:
    code = main(["cores", "--lock", str(lock), *args])
    captured = capsys.readouterr()
    return code, captured.out + captured.err


def test_the_checkout_lock_pins_every_game_core():
    lock = _native.CoresLock.load(cores.LOCK)
    game_cores = {_native.GameConfig.load(path).core for path in GAMES_DIR.glob("*/game.toml")}
    assert game_cores - {NAME} <= set(lock.names)
    assert "fbneo" in lock.names
    plan = cores.plan(lock)
    assert plan["tag"] == lock.release_tag
    assert [core["name"] for core in plan["cores"]] == lock.names


def test_release_lays_out_assets_sums_and_lock(released: tuple[Path, Path], builds: Path):
    out, lock_path = released
    asset = _native.asset_name(NAME, cores.TARGET)
    assert sorted(path.name for path in out.iterdir()) == sorted(
        [asset, "SHA256SUMS", "cores.lock"]
    )
    sha = _native.CoresLock.load(lock_path).artifacts(NAME)[cores.TARGET]
    assert (out / "SHA256SUMS").read_text(encoding="utf-8") == f"{sha}  {asset}\n"
    assert (out / asset).read_bytes() == (
        builds / cores.TARGET / _native.library_file(NAME, cores.TARGET)
    ).read_bytes()
    assert lock_path.read_text(encoding="utf-8").startswith("# Pinned libretro core builds.")


def test_fetch_from_a_directory_checks_hash_and_version(
    released: tuple[Path, Path], tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    out, lock_path = released
    data = tmp_path / "data"
    code, text = run(capsys, lock_path, "fetch", str(data), "--from", str(out))
    assert code == 0, text
    assert f"{NAME}: fetched" in text
    assert "loads and reports" in text
    path = data / "cores" / _native.library_file(NAME, cores.TARGET)
    assert path.is_file()
    assert [p.name for p in (data / "cores").iterdir()] == [path.name]

    code, text = run(capsys, lock_path, "fetch", str(data), "--from", str(tmp_path / "nowhere"))
    assert code == 0, text
    assert "has the pinned hash" in text, "a pinned file isn't fetched again"


def test_fetch_never_puts_a_build_with_the_wrong_hash_in_place(
    released: tuple[Path, Path], tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    out, lock_path = released
    asset = out / _native.asset_name(NAME, cores.TARGET)
    asset.write_bytes(asset.read_bytes() + b"\0")
    data = tmp_path / "data"
    code, text = run(capsys, lock_path, "fetch", str(data), "--from", str(out))
    assert code == 1, text
    assert "but cores.lock pins" in text
    assert list((data / "cores").iterdir()) == []


def test_fetch_checks_the_reported_version(
    released: tuple[Path, Path], tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    out, lock_path = released
    text = lock_path.read_text(encoding="utf-8")
    lock = _native.CoresLock.parse(text, "lock")
    version = lock.core(NAME)["library_version"]
    wrong = tmp_path / "wrong-version.lock"
    wrong.write_text(text.replace(f'"{version}"', '"9.9.9"'), encoding="utf-8")
    code, text = run(capsys, wrong, "fetch", str(tmp_path / "data"), "--from", str(out))
    assert code == 1, text
    assert f"reports library_version {version!r}".replace("'", '"') in text


def test_fetch_needs_the_hashes_of_this_platform(
    tmp_path: Path, version: str, capsys: pytest.CaptureFixture[str]
):
    unpinned = tmp_path / "unpinned.lock"
    unpinned.write_text(lock_text(version), encoding="utf-8")
    code, text = run(capsys, unpinned, "fetch", str(tmp_path / "data"))
    assert code == 2
    assert f"no {cores.TARGET} build of {NAME}" in text
    with pytest.raises(ConfigError):
        cores.fetch(_native.CoresLock.load(unpinned), tmp_path, None, print)


def test_check_loads_each_build(
    builds: Path, tmp_path: Path, version: str, capsys: pytest.CaptureFixture[str]
):
    unpinned = tmp_path / "unpinned.lock"
    unpinned.write_text(lock_text(version), encoding="utf-8")
    code, text = run(capsys, unpinned, "check", str(builds / cores.TARGET))
    assert code == 0, text
    assert f"no {cores.TARGET} hash pinned" in text
    code, text = run(capsys, unpinned, "check", str(tmp_path))
    assert code == 1, text
    assert f"{NAME}: FAILED" in text


def test_plan_prints_json(tmp_path: Path, version: str, capsys: pytest.CaptureFixture[str]):
    lock = tmp_path / "cores.lock"
    lock.write_text(lock_text(version), encoding="utf-8")
    code, text = run(capsys, lock, "plan")
    assert code == 0
    plan = json.loads(text)
    assert plan["cores"] == [
        {
            "name": NAME,
            "repo": "https://example.invalid/gth-testcore",
            "commit": COMMIT,
            "library_version": version,
        }
    ]
    assert plan["tag"].startswith("cores-")


def test_a_broken_lock_is_a_config_error(tmp_path: Path, capsys: pytest.CaptureFixture[str]):
    lock = tmp_path / "cores.lock"
    lock.write_text("[cores.x]\nrepo = 1\n", encoding="utf-8")
    code, text = run(capsys, lock, "plan")
    assert code == 2
    assert "gth cores plan:" in text


def test_fetch_from_a_url(
    released: tuple[Path, Path], tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    out, lock_path = released
    code, text = run(capsys, lock_path, "fetch", str(tmp_path / "data"), "--from", out.as_uri())
    assert code == 0, text
    assert f"{NAME}: fetched" in text


def test_pinned_lock_needs_the_pinned_build(
    released: tuple[Path, Path], builds: Path, tmp_path: Path, testcore_path: Path
):
    _, lock_path = released
    lock = cores.pinned_lock(lock_path, NAME, testcore_path)
    assert lock is not None
    assert lock.core(NAME)["commit"] == COMMIT
    assert cores.pinned_lock(lock_path, "fbneo", testcore_path) is None
    assert cores.pinned_lock(tmp_path / "missing.lock", NAME, testcore_path) is None
    other = tmp_path / "other"
    other.write_bytes(testcore_path.read_bytes() + b"\0")
    assert cores.pinned_lock(lock_path, NAME, other) is None


def test_recordings_keep_the_lock_entry_of_a_pinned_build(
    released: tuple[Path, Path],
    testcore_path: Path,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    from gtharness.env import RetroEnv
    from gtharness.record import Record
    from gtharness.rewards import score_delta

    _, lock_path = released
    game = _native.GameConfig.load(GAMES_DIR / "testcore" / "game.toml")
    options = {
        "core_path": testcore_path,
        "content_path": None,
        "system_dir": tmp_path,
        "frameskip": 1,
        "obs_size": (60, 80),
        "action_format": "flat",
        "reward": score_delta,
        "sticky_prob": 0.0,
        "max_episode_frames": None,
        "noop_max": 0,
        "maxpool": False,
        "grayscale": False,
        "reset_state": None,
        "game_id": "testcore",
    }

    def record_one(directory: Path) -> dict[str, Any]:
        record = Record(
            directory=directory,
            recorder="scripted",
            player="random",
            license="CC0-1.0",
            checks_every=60,
        )
        env = RetroEnv(game, record=record, **options)
        try:
            env.reset()
            env.step(0)
        finally:
            env.close()
        return tomllib.loads((env.recordings[0] / "meta.toml").read_text(encoding="utf-8"))

    monkeypatch.setattr(cores, "LOCK", lock_path)
    lock = record_one(tmp_path / "pinned")["core"]["lock"]
    assert (lock["commit"], lock["repo"]) == (COMMIT, "https://example.invalid/gth-testcore")
    monkeypatch.setattr(cores, "LOCK", tmp_path / "missing.lock")
    assert "lock" not in record_one(tmp_path / "unpinned")["core"]

    core = _native.Core(testcore_path, None, tmp_path, {}, 0, 60, 80, None, 0, "area", False, False)
    try:
        other = _native.CoresLock.parse(
            lock_text("1").replace(f"cores.{NAME}", "cores.other"), "other.lock"
        )
        with pytest.raises(ConfigError, match="no core gth_testcore"):
            core.start_recording(
                tmp_path / "x.gthrec", game, "testcore", other, "policy", "p", "l", 60
            )
    finally:
        core.close()
