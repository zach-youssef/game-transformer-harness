"""RAM search (docs/games.md, "RAM search"): finding a game's RAM variables from a script, and
checking them in game tests. A developer tool: game RAM is never a policy's input.

The search itself is `_native.RamSearch`, in Rust; this module runs it on an environment's
core, formats what it finds as ``[ram]`` tables and is ``gth ram-search``.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any

from gtharness import _native
from gtharness.env import GAMES_DIR, RetroEnv
from gtharness.errors import HarnessError


def run_script(
    env: RetroEnv, script_path: str | os.PathLike[str]
) -> tuple[_native.RamSearch, list[int]]:
    """Runs the search script at ``script_path`` on ``env``'s core from its current state
    (after `RetroEnv.reset`, the game's reset). Returns the search and the number of
    candidates at the start and after each step. The core plays on: `RetroEnv.reset` before
    stepping ``env`` again."""
    return _native.RamSearch.run_script(_core(env), script_path)


def watch(
    env: RetroEnv, search: _native.RamSearch, inputs: list[tuple[int, int]], limit: int
) -> list[list[int | None]]:
    """Runs ``inputs`` (``(buttons, frames)`` pairs, RetroPad button bits) on ``env``'s core and
    returns the first ``limit`` candidates' values after every frame."""
    return search.watch(_core(env), inputs, limit)


def _core(env: RetroEnv) -> _native.Core:
    # RetroEnv has no public core (a policy needs none); this developer tool drives it.
    return env._core


def ram_table(name: str, candidate: Mapping[str, Any]) -> str:
    """``candidate`` (from `_native.RamSearch.candidates`) as a ``game.toml`` RAM variable."""
    if "address" in candidate:
        where = [f"address = {candidate['address']:#x}"]
    else:
        where = [f'memory = "{candidate["memory"]}"', f"offset = {candidate['offset']:#x}"]
    lines = [f"[ram.{name}]", *where, f"size = {candidate['size']}"]
    lines.append(f'encoding = "{candidate["encoding"]}"')
    if candidate["swap16"]:
        lines.append("swap16 = true")
    return "\n".join(lines)


def assert_ram(ram: Mapping[str, int], name: str, expected: int | Callable[[int], bool]) -> None:
    """Asserts that RAM variable ``name`` in ``ram`` (``info["ram"]``) equals ``expected``, or
    satisfies it if it's a predicate, for game tests that validate a ``[ram]`` table against
    known play (a score after a scripted run, lives after a death)."""
    if name not in ram:
        raise AssertionError(f"no RAM variable {name!r} (the game has {sorted(ram)})")
    value = ram[name]
    if callable(expected):
        if not expected(value):
            raise AssertionError(f"RAM variable {name} is {value}, which fails {expected!r}")
    elif value != expected:
        raise AssertionError(f"RAM variable {name} is {value}, expected {expected}")


def add_parser(commands: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    """Adds ``gth ram-search`` to the ``gth`` command line."""
    parser = commands.add_parser(
        "ram-search",
        help="find RAM variables by running a search script (a developer tool)",
        description="Loads the game, resets it, runs the search script (docs/games.md, 'RAM "
        "search') and prints the candidates left, each as a [ram] table. Exit status: 0 when "
        "candidates are left, 1 when none are, 2 on a usage or config error.",
    )
    parser.add_argument("game", help="the game's directory under --games-dir")
    parser.add_argument("data_dir", type=Path, help="data directory, as for RetroEnv.from_game")
    parser.add_argument("script", type=Path, help="the search script (TOML)")
    parser.add_argument(
        "--limit", type=_count, default=20, help="candidates to print (default: 20)"
    )
    parser.add_argument(
        "--watch",
        type=_frames,
        metavar="FRAMES",
        help="then run FRAMES frames with no button held and print the shown candidates' "
        "values after each",
    )
    parser.add_argument(
        "--rom-dir", type=Path, help="default: GTH_ROM_DIR or rom_dir in the user config"
    )
    parser.add_argument("--core-path", type=Path, help="default: data_dir/cores/")
    parser.add_argument("--games-dir", type=Path, default=GAMES_DIR)
    parser.add_argument("--json", action="store_true", help="print the result as JSON")


def _count(text: str) -> int:
    """An argument that must be a whole number (argparse exits with status 2 otherwise)."""
    try:
        value = int(text)
    except ValueError:
        value = -1
    if value < 0:
        raise argparse.ArgumentTypeError(f"must be a whole number >= 0, not {text!r}")
    return value


def _frames(text: str) -> int:
    """A frame count: a whole number that fits the core's (unsigned 32-bit) frame counts. The
    search refuses more than it watches at once with a clear error."""
    value = _count(text)
    if value >= 1 << 32:
        raise argparse.ArgumentTypeError(f"{text} frames is too many")
    return value


def search(args: argparse.Namespace) -> dict[str, Any]:
    """``gth ram-search``'s result for ``args``: the step counts, candidates and watch."""
    env = RetroEnv.from_game(
        args.game,
        args.data_dir,
        frameskip=1,
        obs_size=(16, 16),  # unused: the search reads memory, not frames
        action_format="flat",
        reward=lambda previous, current: 0.0,
        sticky_prob=0.0,
        max_episode_frames=None,
        noop_max=0,
        maxpool=False,
        grayscale=False,
        rom_dir=args.rom_dir,
        core_path=args.core_path,
        games_dir=args.games_dir,
    )
    try:
        env.reset()
        found, counts = run_script(env, args.script)
        result: dict[str, Any] = {
            "space": found.space,
            "views": found.views,
            "counts": counts,
            "left": len(found),
            "candidates": found.candidates(args.limit),
        }
        if args.watch is not None:
            result["watch"] = watch(env, found, [(0, args.watch)], args.limit)
        return result
    finally:
        env.close()


def table(result: dict[str, Any]) -> str:
    """``result`` as text: the counts, then each candidate as a ``[ram]`` table."""
    counts = " -> ".join(str(count) for count in result["counts"])
    lines = [f"{result['space']}, views {', '.join(result['views'])}: {counts} candidates"]
    shown = result["candidates"]
    if len(shown) < result["left"]:
        lines.append(f"showing {len(shown)} of {result['left']} (--limit)")
    for number, candidate in enumerate(shown):
        lines += ["", f"# {candidate['view']}, value {candidate['value']}"]
        lines.append(ram_table(f"candidate_{number}", candidate))
    if "watch" in result:
        lines += ["", "frame  " + "  ".join(f"cand_{n}" for n in range(len(shown)))]
        for frame, values in enumerate(result["watch"], 1):
            cells = "  ".join(f"{'-' if v is None else v:>6}" for v in values)
            lines.append(f"{frame:>5}  {cells}")
    return "\n".join(lines)


def main(args: argparse.Namespace) -> int:
    """Runs ``gth ram-search``; returns its exit status."""
    try:
        result = search(args)
    except HarnessError as error:
        print(f"gth ram-search: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2) if args.json else table(result))
    return 0 if result["left"] else 1
