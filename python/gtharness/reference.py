"""Human reference scores (docs/evaluation.md, "Human reference"): `reference_scores` and
``gth reference-scores`` read a score from each human recording of one credit, under an
evaluation protocol's rules for a human, by replaying it.

A reference credit is a recording by ``recorder = "human"`` with no ``[[rewind]]`` and no
``[branch]``, whose start state is the game's reset (a fresh core's state after the reset
script or savestate, compared by SHA-1), replayed until the game's ``episode.terminate``
first holds, or the protocol's ``max_episode_frames`` (then it is truncated, as an agent's
episode would be). Its score is the protocol's ``score_var`` there minus at the start. A
recording that ends before either is not a whole credit, and is refused, as is anything else
that breaks the rule: the output only ever holds credits that follow it.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
import statistics
import sys
import tomllib
from collections.abc import Sequence
from pathlib import Path
from typing import Any

from gtharness import _native
from gtharness import protocol as protocol_module
from gtharness.env import GAMES_DIR, core_library, find_content
from gtharness.errors import ConfigError, HarnessError
from gtharness.evaluate import check_protocol, protocol_path
from gtharness.protocol import EvalProtocol

__all__ = ["MIN_CREDITS", "REFERENCE_FORMAT", "reference_scores"]

REFERENCE_FORMAT = 1
"""The output's ``format``; bumped on any change to its fields."""

MIN_CREDITS = 5
"""Fewer credits of a game than this is a warning in the output (a mean of so few is noisy),
not an error."""


def reference_scores(
    protocol: EvalProtocol,
    recordings: Sequence[str | os.PathLike[str]],
    data_dir: str | os.PathLike[str],
    *,
    rom_dir: str | os.PathLike[str] | None,
    core_path: str | os.PathLike[str] | None,
    games_dir: str | os.PathLike[str],
) -> dict[str, Any]:
    """The human reference scores of ``recordings`` under ``protocol``: per game of the
    protocol, the credits' scores and their count, mean and median. Raises ConfigError naming
    the first recording that isn't a reference credit (module docstring), and the protocol
    checks' errors (`check_protocol`)."""
    if protocol.life_loss_terminal:
        raise ConfigError(
            "life_loss_terminal: a reference credit runs to game over, and the protocol ends"
            " episodes at a life lost"
        )
    by_game: dict[str, list[tuple[Path, dict[str, Any]]]] = {
        game_id: [] for game_id in protocol.games
    }
    seen: dict[str, Path] = {}
    for recording in recordings:
        path = Path(recording)
        meta = _check_meta(path, protocol)
        if path.name in seen:
            raise ConfigError(f"{path}: recording {path.name} is given twice ({seen[path.name]})")
        seen[path.name] = path
        by_game[meta["game_id"]].append((path, meta))
    # Only the games with credits need their files. The action set is the agent's; a person
    # plays on the whole pad, so it doesn't apply.
    played = tuple(game_id for game_id, paths in by_game.items() if paths)
    check_protocol(
        dataclasses.replace(protocol, games=played, action_set="harness"),
        data_dir,
        rom_dir=rom_dir,
        core_path=core_path,
        games_dir=games_dir,
    )

    data = Path(data_dir)
    games: dict[str, Any] = {}
    for game_id, paths in by_game.items():
        if not paths:
            games[game_id] = _summary([])
            continue
        config = _native.GameConfig.load(Path(games_dir) / game_id / "game.toml")
        content = find_content(config, game_id, rom_dir)
        library = Path(core_path) if core_path else core_library(data, config.core)
        system = data / "system"
        start = _reset_sha1(config, game_id, library, content, system, data)
        credits = [
            _credit(path, meta, config, start, protocol, library, content, system)
            for path, meta in sorted(paths, key=lambda entry: entry[0].name)
        ]
        games[game_id] = _summary(credits)
    expanded = protocol.expand()
    return {
        "format": REFERENCE_FORMAT,
        "protocol": {
            "sha1": expanded.sha1,
            "text": expanded.text,
            # What applies to a person: where a credit starts, where it is cut, what scores.
            "human": {
                "start_reset": protocol.start.reset,
                "max_episode_frames": protocol.max_episode_frames,
                "score_var": protocol.score_var,
            },
        },
        "games": games,
    }


def _summary(credits: list[dict[str, Any]]) -> dict[str, Any]:
    """A game's entry in the output, from its credits."""
    scores = [credit["score"] for credit in credits]
    game: dict[str, Any] = {
        "n": len(credits),
        "mean": statistics.fmean(scores) if scores else None,
        "median": statistics.median(scores) if scores else None,
        "recordings": [credit["recording"] for credit in credits],
        "credits": credits,
    }
    if len(credits) < MIN_CREDITS:
        game["warning"] = (
            f"{len(credits)} credits, fewer than {MIN_CREDITS}: the mean and median are noisy"
        )
    return game


def _check_meta(path: Path, protocol: EvalProtocol) -> dict[str, Any]:
    """The recording's ``meta.toml``, after the checks it alone settles."""
    meta_path = path / "meta.toml"
    try:
        meta = tomllib.loads(meta_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise ConfigError(
            f"{path}: cannot read meta.toml (an incomplete recording?): {error}"
        ) from error
    if meta.get("recorder") != "human":
        raise ConfigError(
            f"{path}: recorder {meta.get('recorder')!r}: a reference credit is recorder 'human'"
        )
    if "rewind" in meta:
        raise ConfigError(
            f"{path}: has [[rewind]]: a rewound recording isn't one credit (record with"
            " gth-play --rewind-seconds 0)"
        )
    if "branch" in meta:
        raise ConfigError(f"{path}: has [branch]: a rewind clip isn't a credit")
    game_id = meta.get("game_id")
    if game_id not in protocol.games:
        raise ConfigError(f"{path}: game {game_id!r} isn't one of the protocol's {protocol.games}")
    start = meta.get("start")
    if not isinstance(start, dict) or not isinstance(start.get("sha1"), str):
        raise ConfigError(f"{path}: meta.toml has no [start] sha1")
    if not isinstance(meta.get("player"), str):
        raise ConfigError(f"{path}: meta.toml has no player")
    return meta


def _reset_sha1(
    config: _native.GameConfig,
    game_id: str,
    library: Path,
    content: Path,
    system: Path,
    data: Path,
) -> str:
    """SHA-1 of a fresh core's state after the game's reset: where `RetroEnv` starts an episode
    and records from, before its no-ops."""
    state = None
    if config.reset_state is not None:
        try:
            state = (data / "states" / game_id / config.reset_state).read_bytes()
        except OSError as error:
            raise ConfigError(f"{game_id}: cannot read the reset state: {error}") from error
        config.check_reset_state(state)
    core = _native.Core(
        library,
        content,
        system,
        config.core_options,
        config.settle_frames,
        1,
        1,
        None,
        0,
        "area",
        False,
        False,
    )
    try:
        core.reset()
        if state is None:
            core.run_inputs(config.reset_inputs or [])
        else:
            core.unserialize(state)
        return hashlib.sha1(core.serialize()).hexdigest()
    finally:
        core.close()


def _credit(
    path: Path,
    meta: dict[str, Any],
    config: _native.GameConfig,
    reset_sha1: str,
    protocol: EvalProtocol,
    library: Path,
    content: Path,
    system: Path,
) -> dict[str, Any]:
    """One reference credit's score, by replay (verifying every desync check)."""
    if meta["start"]["sha1"] != reset_sha1:
        raise ConfigError(
            f"{path}: doesn't start at the game's reset (start state SHA-1"
            f" {meta['start']['sha1']}, the reset's {reset_sha1}): record with gth-play"
            " --record-from-reset"
        )
    replay = _native.Replayer(path, library, content, system, 1, 1, None, 0, "area")
    try:
        _, start, _ = replay.run_until_terminated(config, 0)
        cap = protocol.max_episode_frames
        frames, end, terminated = replay.run_until_terminated(config, cap)
        if not terminated and frames < cap:
            raise ConfigError(
                f"{path}: ends after {frames} frames, before the game's episode.terminate holds"
                f" or max_episode_frames ({cap}): not a whole credit (play to game over)"
            )
    finally:
        replay.close()
    score_var = protocol.score_var
    return {
        "recording": path.name,
        "player": meta["player"],
        "score": end[score_var] - start[score_var],
        "frames": frames,
        "terminated": terminated,
        "truncated": not terminated,
    }


def add_parser(commands: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    """Adds ``gth reference-scores`` to the ``gth`` command line."""
    parser = commands.add_parser(
        "reference-scores",
        help="score human recordings of one credit under an evaluation protocol",
        description="Replays each RECORDING (a human credit from the game's reset, gth-play "
        "--record-from-reset --rewind-seconds 0) to where the game's episode first terminates "
        "or PROTOCOL's max_episode_frames, and writes each game's scores, count, mean and "
        "median to OUT as JSON (docs/evaluation.md, 'Human reference'). Exit status: 0 when "
        "written, 2 when a recording isn't a reference credit or on a usage or config error.",
    )
    parser.add_argument("data_dir", type=Path, help="data directory, as for RetroEnv.from_game")
    parser.add_argument("recordings", type=Path, nargs="+", metavar="RECORDING")
    parser.add_argument(
        "--protocol",
        required=True,
        help="a protocol file, or a preset's name in protocols/ (e.g. ours-v1)",
    )
    parser.add_argument("--out", type=Path, required=True, help="the JSON file to write")
    parser.add_argument(
        "--rom-dir", type=Path, help="default: GTH_ROM_DIR or rom_dir in the user config"
    )
    parser.add_argument("--core-path", type=Path, help="default: data_dir/cores/")
    parser.add_argument("--games-dir", type=Path, default=GAMES_DIR)


def main(args: argparse.Namespace) -> int:
    """Runs ``gth reference-scores``; returns its exit status."""
    try:
        result = reference_scores(
            protocol_module.load(protocol_path(args.protocol)),
            args.recordings,
            args.data_dir,
            rom_dir=args.rom_dir,
            core_path=args.core_path,
            games_dir=args.games_dir,
        )
        text = json.dumps(result, indent=2) + "\n"
        try:
            args.out.write_text(text, encoding="utf-8")
        except OSError as error:
            raise ConfigError(f"cannot write {args.out}: {error}") from error
    except HarnessError as error:
        print(f"gth reference-scores: {error}", file=sys.stderr)
        return 2
    for game_id, game in result["games"].items():
        line = f"{game_id}: {game['n']} credits, mean {game['mean']}, median {game['median']}"
        print(line + (f" (warning: {game['warning']})" if "warning" in game else ""))
    return 0
