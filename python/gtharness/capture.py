"""Scripted captures (docs/recording-format.md, "Scripted captures"): `capture` and
``gth capture`` record a game from its reset with a named input script, no player involved,
as a ``scripted`` recording that exports to shards like any other.

The script only chooses the inputs: it is turned into one RetroPad mask per step up front and
played by `_native.Core.run_inputs` (all frames in Rust, through the recorder), so the inputs
in ``inputs.bin`` are what replays, and the script's name, version and seed in the recording's
``player`` only say where they came from.
"""

from __future__ import annotations

import argparse
import sys
from collections.abc import Callable
from pathlib import Path

import numpy as np

from gtharness import _native
from gtharness.env import GAMES_DIR, RetroEnv
from gtharness.errors import ConfigError, HarnessError
from gtharness.record import Record

SCRIPTS = ("attract", "random-play")
"""The input scripts (docs/recording-format.md, "Scripted captures")."""
SCRIPT_VERSIONS = {"attract": 1, "random-play": 1}
"""Each script's version, part of ``player``: bumped whenever its inputs for the same
arguments (and the same game.toml and numpy) change."""

SELECT = 1 << 2
"""RetroPad select (RETRO_DEVICE_ID_JOYPAD_SELECT): the coin on arcade cores."""
START = 1 << 3
"""RetroPad start (RETRO_DEVICE_ID_JOYPAD_START)."""

# random-play, version 1. Lengths are frames, rounded up to whole steps.
CREDIT_EVERY = 480
"""Frames between two credit cycles (coin, then start), from the first step on: they start a
game from attract mode and continue one at game over (every continue countdown seen
lasts longer)."""
PULSE = 8
"""Frames the coin and the start button are held."""
GAP = 16
"""Frames with nothing held after each of them."""
HOLD = (8, 48)
"""The shortest and longest hold of one random action (inclusive)."""
P_SHOT = 0.8
"""Probability that a hold has the shot slot held."""
P_BOMB = 0.05
"""Probability that a hold has the bomb slot held (when the game maps one)."""


def player_name(script: str, seed: int | None) -> str:
    """The recording's ``player`` for ``script``: its name and version, and the seed of a
    seeded script (``random-play-v1 seed=7``, ``attract-v1``)."""
    name = f"{script}-v{SCRIPT_VERSIONS[script]}"
    return name if seed is None else f"{name} seed={seed}"


def script_steps(
    config: _native.GameConfig, script: str, steps: int, frameskip: int, seed: int | None
) -> list[int]:
    """``steps`` RetroPad masks, one per step of ``frameskip`` frames, that ``script`` plays on
    the game ``config``. Raises ConfigError for an unknown script, a seed given to
    ``attract`` or missing for ``random-play``, or a game with no shot slot for
    ``random-play``."""
    if script not in SCRIPTS:
        raise ConfigError(f"script must be one of {list(SCRIPTS)}, not {script!r}")
    if script == "attract":
        if seed is not None:
            raise ConfigError("the attract script takes no seed: it holds nothing")
        return [0] * steps
    if seed is None:
        raise ConfigError("the random-play script needs a seed")
    return _random_play(config, steps, frameskip, seed)


def _random_play(config: _native.GameConfig, steps: int, frameskip: int, seed: int) -> list[int]:
    if "shot" not in config.mapped_slots:
        raise ConfigError("random-play needs a game that maps the shot slot")
    shot = 1 << config.slots.index("shot")
    bomb = 1 << config.slots.index("bomb") if "bomb" in config.mapped_slots else 0
    table = config.joypad_table()
    rng = np.random.default_rng(seed)

    def to_steps(frames: int) -> int:
        return -(-frames // frameskip)

    credit = (
        [SELECT] * to_steps(PULSE)
        + [0] * to_steps(GAP)
        + [START] * to_steps(PULSE)
        + [0] * to_steps(GAP)
    )
    credit_every = to_steps(CREDIT_EVERY)
    masks: list[int] = []
    next_credit = 0
    while len(masks) < steps:
        if len(masks) >= next_credit:
            masks += credit
            next_credit += credit_every
            continue
        hold = to_steps(int(rng.integers(HOLD[0], HOLD[1] + 1)))
        direction = int(rng.integers(0, 9))
        buttons = (shot if rng.random() < P_SHOT else 0) | (bomb if rng.random() < P_BOMB else 0)
        masks += [table[config.encode(direction, buttons)]] * min(hold, next_credit - len(masks))
    return masks[:steps]


def capture(
    game_id: str,
    data_dir: str | Path,
    out_dir: str | Path,
    *,
    script: str,
    frames: int,
    frameskip: int,
    seed: int | None,
    license: str,
    checks_every: int,
    rom_dir: str | Path | None = None,
    core_path: str | Path | None = None,
    games_dir: str | Path = GAMES_DIR,
    report: Callable[[str], None] | None = None,
) -> Path:
    """Records ``frames`` frames of ``game_id`` from its reset, ``script`` choosing the inputs
    (`script_steps`), in steps of ``frameskip`` frames, into a new recording under
    ``out_dir``, and returns it. The game and its files are found as `RetroEnv.from_game`
    finds them; the recording is ``scripted``, its player is `player_name`, and ``license``
    and ``checks_every`` are as in `gtharness.record.Record`. ``frames`` counts the reset's
    first observation's frames too, so it is a multiple of ``frameskip`` (ConfigError
    otherwise). ``report`` gets a line of progress about every minute of game time. A capture
    that fails or is interrupted leaves its recording incomplete (no ``meta.toml``)."""
    if isinstance(frameskip, bool) or frameskip < 1:
        raise ConfigError(f"frameskip must be at least 1, not {frameskip}")
    if checks_every < 1:
        raise ConfigError(f"checks_every must be at least 1, not {checks_every}")
    if frames < 2 * frameskip or frames % frameskip:
        raise ConfigError(
            f"frames must be a multiple of frameskip ({frameskip}) of at least two steps,"
            f" not {frames}"
        )
    config = _native.GameConfig.load(Path(games_dir) / game_id / "game.toml")
    masks = script_steps(config, script, frames // frameskip - 1, frameskip, seed)
    env = RetroEnv.from_game(
        game_id,
        data_dir,
        frameskip=frameskip,
        obs_size=(16, 16),  # nothing looks at the observations
        action_format="flat",
        reward=_no_reward,
        rom_dir=rom_dir,
        core_path=core_path,
        games_dir=games_dir,
        record=Record(
            directory=out_dir,
            recorder="scripted",
            player=player_name(script, seed),
            license=license,
            checks_every=checks_every,
        ),
    )
    try:
        env.reset()
        fps = env._core.av_info()["fps"]
        chunk = max(1, round(60 * fps / frameskip))
        for start in range(0, len(masks), chunk):
            # A developer tool drives the core directly, as gtharness.ramsearch does: coin and
            # start are no action, and run_inputs plays a chunk in one call.
            env._core.run_inputs([(mask, frameskip) for mask in masks[start : start + chunk]])
            if report is not None:
                done = (start + len(masks[start : start + chunk]) + 1) * frameskip
                report(f"{game_id}: {done}/{frames} frames")
    except BaseException:
        env._core.close()  # unfinished: the recording stays incomplete
        raise
    finally:
        env.close()
    (recording,) = env.recordings
    return recording


def _no_reward(_previous: dict[str, int], _current: dict[str, int]) -> float:
    return 0.0


def add_parser(commands: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    """Adds ``gth capture`` to the ``gth`` command line."""
    parser = commands.add_parser(
        "capture",
        help="record a game from its reset with an input script",
        description="Records FRAMES frames of GAME from its reset, the input script choosing "
        "every step's buttons, as a 'scripted' recording under OUT (docs/recording-format.md, "
        "'Scripted captures'). Exit status: 0 when the recording is complete, 2 on a usage, "
        "config or core error.",
    )
    parser.add_argument("data_dir", type=Path, help="data directory, as for RetroEnv.from_game")
    parser.add_argument("game", help="the game's directory under games/")
    parser.add_argument("--out", type=Path, required=True, help="directory for the recording")
    parser.add_argument("--script", choices=SCRIPTS, required=True)
    parser.add_argument(
        "--frames", type=int, required=True, help="frames to record, a multiple of --frameskip"
    )
    parser.add_argument("--frameskip", type=int, required=True, help="frames per recorded step")
    parser.add_argument("--seed", type=int, help="random-play's seed (attract takes none)")
    parser.add_argument("--license", required=True, help="e.g. CC-BY-NC-4.0")
    parser.add_argument(
        "--checks-every", type=int, required=True, help="frames between desync checks"
    )
    parser.add_argument(
        "--rom-dir", type=Path, help="default: GTH_ROM_DIR or rom_dir in the user config"
    )
    parser.add_argument("--core-path", type=Path, help="default: data_dir/cores/")
    parser.add_argument("--games-dir", type=Path, default=GAMES_DIR)


def main(args: argparse.Namespace) -> int:
    """Runs ``gth capture``; returns its exit status."""
    try:
        recording = capture(
            args.game,
            args.data_dir,
            args.out,
            script=args.script,
            frames=args.frames,
            frameskip=args.frameskip,
            seed=args.seed,
            license=args.license,
            checks_every=args.checks_every,
            rom_dir=args.rom_dir,
            core_path=args.core_path,
            games_dir=args.games_dir,
            report=lambda line: print(line, file=sys.stderr, flush=True),
        )
    except (HarnessError, ValueError) as error:
        print(f"gth capture: {error}", file=sys.stderr)
        return 2
    print(recording)
    return 0
