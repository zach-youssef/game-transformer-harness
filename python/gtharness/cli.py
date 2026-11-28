"""The ``gth`` command line. ``gth audit``: the ROM audit (docs/roms.md, "Auditing");
``gth ram-search``: RAM search (docs/games.md, "RAM search"); ``gth export-shards``: training
shards (docs/recording-format.md, "Training shards"); ``gth cores``: the pinned core builds
(docs/cores.md); ``gth bench``: benchmarks of real cores (docs/performance.md); ``gth capture``:
scripted captures (docs/recording-format.md, "Scripted captures"); ``gth evaluate``: a policy
under an evaluation protocol (docs/evaluation.md, "Running an evaluation")."""

from __future__ import annotations

import argparse
import json
import sys
from collections.abc import Sequence
from pathlib import Path
from typing import Any

from gtharness import _native, bench, capture, cores, evaluate, ramsearch, shards
from gtharness.env import GAMES_DIR
from gtharness.errors import ConfigError

TESTCORE = "gth_testcore"
"""The test core's games, whose files tests write, are audited only when named."""


def audit(
    data_dir: Path, rom_dir: Path, games_dir: Path, games: Sequence[str] | None
) -> dict[str, Any]:
    """The audit of ``games`` (default: every game in ``games_dir`` but the test core's), with
    content under ``rom_dir`` and BIOS files under ``data_dir/system``. Raises ConfigError if
    there's no game to audit."""
    system = data_dir / "system"
    if games:
        ids = list(games)
    else:
        ids = sorted(path.parent.name for path in games_dir.glob("*/game.toml"))
    report: dict[str, Any] = {"rom_dir": str(rom_dir), "system_dir": str(system), "games": {}}
    for game_id in ids:
        config = _native.GameConfig.load(games_dir / game_id / "game.toml")
        if config.core == TESTCORE and not games:
            continue
        report["games"][game_id] = json.loads(config.audit(rom_dir, system))
    if not report["games"]:
        raise ConfigError(
            f"no games to audit in {games_dir} (the test core's are audited only with --game)"
        )
    return report


def table(report: dict[str, Any]) -> str:
    """The report as text: one line per game and per file, and one per member that isn't ok."""
    lines = [f"ROM directory: {report['rom_dir']}", f"System directory: {report['system_dir']}"]
    for game_id, game in report["games"].items():
        lines.append(f"{game_id}: {game['status']}")
        for file in game["files"]:
            where = f"  {file['path']}" if "path" in file else ""
            lines.append(f"  {file['role']:<8} {file['status']:<10} {file['file']}{where}")
            if "found" in file:
                lines.append(f"    found {_hashes(file['found'])}")
            if "error" in file:
                lines.append(f"    {file['error']}")
            for member in file.get("members", []):
                if member["status"] != "ok":
                    detail = f" ({member['error']})" if "error" in member else ""
                    detail += f" found {_hashes(member['found'])}" if "found" in member else ""
                    lines.append(f"    {member['status']:<10} {member['name']}{detail}")
            if file.get("extra"):
                broken = sum("error" in entry for entry in file["extra"])
                note = f" ({broken} unreadable)" if broken else ""
                lines.append(f"    {len(file['extra'])} other members{note}")
    return "\n".join(lines)


def _hashes(found: dict[str, Any]) -> str:
    return f"size {found['size']} crc32 {found['crc32']} sha1 {found['sha1']}"


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="gth", description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    audit_parser = commands.add_parser(
        "audit",
        help="check every game's content, parent and BIOS files by hash",
        description="Checks each game's files and archive members by size, CRC-32 and SHA-1. "
        "Exit status: 0 when every audited game is ok, 1 otherwise, 2 on a usage or config "
        "error.",
    )
    audit_parser.add_argument("data_dir", type=Path, help="data directory (BIOS in system/)")
    audit_parser.add_argument(
        "--rom-dir", type=Path, help="default: GTH_ROM_DIR or rom_dir in the user config"
    )
    audit_parser.add_argument("--games-dir", type=Path, default=GAMES_DIR)
    audit_parser.add_argument(
        "--game", action="append", dest="games", help="audit only this game (repeatable)"
    )
    audit_parser.add_argument("--json", action="store_true", help="print the report as JSON")
    ramsearch.add_parser(commands)
    shards.add_parser(commands)
    bench.add_parser(commands)
    capture.add_parser(commands)
    evaluate.add_parser(commands)
    _add_cores_parser(commands)
    args = parser.parse_args(argv)
    if args.command == "ram-search":
        return ramsearch.main(args)
    if args.command == "export-shards":
        return shards.main(args)
    if args.command == "bench":
        return bench.main(args)
    if args.command == "capture":
        return capture.main(args)
    if args.command == "evaluate":
        return evaluate.main(args)
    if args.command == "cores":
        return _cores_main(args)

    try:
        rom_dir = args.rom_dir if args.rom_dir is not None else _native.rom_dir()
        if rom_dir is None:
            raise ConfigError(
                "no ROM directory: pass --rom-dir, or set GTH_ROM_DIR or rom_dir in the user"
                f" config ({_native.user_config_path()})"
            )
        if not Path(rom_dir).is_dir():
            raise ConfigError(f"the ROM directory {rom_dir} doesn't exist")
        report = audit(args.data_dir, rom_dir, args.games_dir, args.games)
    except ConfigError as error:
        print(f"gth audit: {error}", file=sys.stderr)
        return 2
    print(json.dumps(report, indent=2) if args.json else table(report))
    return 0 if all(game["status"] == "ok" for game in report["games"].values()) else 1


def _add_cores_parser(commands: Any) -> None:
    parser = commands.add_parser(
        "cores",
        help="fetch and check the pinned core builds in cores.lock",
        description="The pinned libretro core builds (docs/cores.md). Exit status: 0 when "
        "every core passes, 1 otherwise, 2 on a usage or config error.",
    )
    parser.add_argument(
        "--lock", type=Path, default=cores.LOCK, help="default: the checkout's cores.lock"
    )
    actions = parser.add_subparsers(dest="action", required=True)
    fetch = actions.add_parser(
        "fetch",
        help="fetch this platform's builds into DATA_DIR/cores and check them",
        description="Fetches this platform's build of every core into DATA_DIR/cores, checks "
        "its SHA-256 against the lock, then loads it without a game and checks the "
        "library_version it reports.",
    )
    fetch.add_argument("data_dir", type=Path, help="data directory (cores go in cores/)")
    fetch.add_argument(
        "--from",
        dest="source",
        help="a directory or URL with the release assets (default: the lock's GitHub release)",
    )
    check = actions.add_parser(
        "check",
        help="load each core's library in a directory and check its version",
        description="Loads each core's library in CORES_DIR (<name>_libretro.<ext>) without a "
        "game and checks the library_version it reports; the cores workflow runs this on "
        "fresh builds. Hashes are reported, not required.",
    )
    check.add_argument("cores_dir", type=Path)
    actions.add_parser("plan", help="print the release tag and each core's source as JSON")
    release = actions.add_parser(
        "release",
        help="lay out a release from the cores workflow's builds",
        description="Copies BUILDS/<target>/<name>_libretro.<ext> for every core and target "
        "into OUT under its asset name, with SHA256SUMS and cores.lock with the hashes "
        "written in.",
    )
    release.add_argument("builds", type=Path)
    release.add_argument("--out", type=Path, required=True)
    release.add_argument(
        "--target", action="append", dest="targets", required=True, help="repeatable"
    )


def _cores_main(args: argparse.Namespace) -> int:
    def report(line: str) -> None:
        print(line, flush=True)

    try:
        lock = _native.CoresLock.load(args.lock)
        if args.action == "plan":
            print(json.dumps(cores.plan(lock)))
            return 0
        if args.action == "release":
            filled = cores.release(lock, args.builds, args.out, args.targets)
            print(f"{filled.release_tag}: {args.out}")
            return 0
        if args.action == "fetch":
            failed = cores.fetch(lock, args.data_dir, args.source, report)
        else:
            failed = cores.check(lock, args.cores_dir, report)
    except (ConfigError, OSError) as error:
        print(f"gth cores {args.action}: {error}", file=sys.stderr)
        return 2
    if failed:
        print(f"gth cores {args.action}: failed: {', '.join(failed)}", file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
