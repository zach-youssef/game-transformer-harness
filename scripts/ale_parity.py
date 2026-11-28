"""Plays the same Atari actions through ale-py and through the harness's `ale` core and
compares them frame by frame (docs/cores.md, "ALE parity").

ale-py runs as ALE v5's environments configure it, minus their wrappers: random seed 0,
``repeat_action_probability`` 0, frame skip 1, the ROM's default mode and difficulty, and
``reset_game()`` after loading, as gymnasium's first ``reset()`` does. The harness runs the
game's config through `RetroEnv` at frame skip 1, no sticky actions, no no-ops, the native
160x210 frame and ``later_episodes`` "reload" (or "chain" with ``--reset-game``). Each frame,
for every game:

- the RGB frame's SHA-1, ALE's reward (the harness's score change), ``lives()`` and
  ``game_over()`` must match;
- the core's episode frame counter (its synthetic region, docs/cores.md) must be ALE's
  ``getEpisodeFrameNumber()`` and its frame counter since the load ``getFrameNumber()``, so
  the reset and the first frame happen at the same emulated frame;
- after a game over both start the next episode and play on: the harness resets to its load
  state, and ale-py reloads the ROM and runs ``reset_game()``, which is the same state. With
  ``--reset-game`` ale-py runs ``reset_game()`` alone, as ALE v5's environments do, and the
  harness chains (``later_episodes = "chain"``: the core's ``retro_reset``, which is ALE's
  ``reset_game()``), so both start the next episode where the last one ended (docs/cores.md,
  "ALE parity").

The actions are uniform over ALE's 18 (`GameConfig.from_atari` on the harness side), held for
a random 1-8 frames, from ``--seed``. Run with ale-py importable next to the harness, its
bundled ROMs as the ROM directory (docs/roms.md, "Atari ROMs"), for example from the venv that
section installs:

    PYTHONPATH=../alepy/lib/python3.12/site-packages uv run python scripts/ale_parity.py data

Prints a line per game and exits 1 if any game differs, with its first difference.
"""

from __future__ import annotations

import argparse
import hashlib
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from gtharness import _native
from gtharness.env import GAMES_DIR, RetroEnv
from gtharness.rewards import score_delta

# The core's frame counters in its synthetic region (the fork's libretro/README.md), read
# here only to compare timing; game configs don't carry them.
COUNTERS = """
[ram.episode_frame]
address = 0x10010
size = 4
encoding = "le"

[ram.frame]
address = 0x10014
size = 4
encoding = "le"

[episode]"""


@dataclass
class Result:
    game_id: str
    frames: int = 0
    episodes: int = 0
    score: int = 0
    difference: str | None = None


def _sha1(frame: np.ndarray) -> str:
    return hashlib.sha1(np.ascontiguousarray(frame).tobytes()).hexdigest()


def compare(
    game_id: str,
    ale_py,
    rom_dir: Path,
    data_dir: Path,
    core_path: Path | None,
    games_dir: Path,
    frames: int,
    seed: int,
    reload: bool,
) -> Result:
    result = Result(game_id)
    config = _native.GameConfig.load(GAMES_DIR / game_id / "game.toml")
    content = config.find_content(rom_dir)
    if content is None:
        result.difference = f"no {config.content_file} in {rom_dir}"
        return result
    toml = (GAMES_DIR / game_id / "game.toml").read_text(encoding="utf-8")
    (games_dir / game_id).mkdir(parents=True)
    (games_dir / game_id / "game.toml").write_text(
        toml.replace("\n[episode]", COUNTERS, 1), encoding="utf-8"
    )

    ale = ale_py.ALEInterface()
    ale.setInt("random_seed", 0)
    ale.setFloat("repeat_action_probability", 0.0)
    ale.setInt("frame_skip", 1)
    ale.loadROM(str(content))
    ale.reset_game()
    actions = ale_py.Action
    minimal = [action.value for action in ale.getMinimalActionSet()]
    if minimal != config.atari_minimal_actions:
        result.difference = (
            f"minimal action set: ale-py {minimal}, game.toml {config.atari_minimal_actions}"
        )
        return result

    env = RetroEnv.from_game(
        game_id,
        data_dir,
        frameskip=1,
        obs_size=(210, 160),
        action_format="flat",
        reward=score_delta,
        sticky_prob=0.0,
        max_episode_frames=None,
        noop_max=0,
        later_episodes="reload" if reload else "chain",
        maxpool=False,
        grayscale=False,
        rom_dir=rom_dir,
        core_path=core_path,
        games_dir=games_dir,
    )
    rng = np.random.default_rng(seed)
    pending: list[int] = []

    def check(step: str, obs: np.ndarray, info: dict, reward: int, ours: float | None) -> bool:
        ram = info["ram"]
        checks = [
            ("frame SHA-1", _sha1(ale.getScreenRGB()), _sha1(obs)),
            ("lives", ale.lives(), ram["lives"]),
            ("game over", int(ale.game_over()), ram["game_over"]),
            ("episode frame", ale.getEpisodeFrameNumber(), ram["episode_frame"]),
            ("frame number", ale.getFrameNumber(), ram["frame"]),
        ]
        if ours is not None:
            checks.append(("reward", reward, ours))
        for what, theirs, harness in checks:
            if theirs != harness:
                result.difference = (
                    f"episode {result.episodes}, {step}: {what}: ale-py {theirs!r},"
                    f" harness {harness!r}"
                )
                return False
        return True

    try:
        while result.frames < frames:
            # RetroEnv.reset runs one frame with no button held for its first observation;
            # ALE gets the same no-op after its reset.
            if result.episodes == 0:
                obs, info = env.reset(seed=seed)
            else:
                if reload:
                    ale.loadROM(str(content))
                ale.reset_game()
                obs, info = env.reset()
            reward = ale.act(actions(0))
            result.episodes += 1
            if not check("reset", obs, info, reward, None):
                return result
            terminated = False
            while not terminated and result.frames < frames:
                if not pending:
                    pending = [int(rng.integers(18))] * int(rng.integers(1, 9))
                atari = pending.pop()
                reward = ale.act(actions(atari))
                obs, ours, terminated, truncated, info = env.step(config.from_atari(atari))
                result.frames += 1
                result.score += int(reward)
                step = f"episode frame {ale.getEpisodeFrameNumber()} (action {atari})"
                if not check(step, obs, info, reward, ours):
                    return result
                if terminated != ale.game_over() or truncated:
                    result.difference = (
                        f"episode {result.episodes}, {step}: terminated {terminated},"
                        f" truncated {truncated}, ale-py game over {ale.game_over()}"
                    )
                    return result
        return result
    finally:
        env.close()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("data_dir", type=Path, help="data directory (the ale core in cores/)")
    parser.add_argument("--core-path", type=Path, help="another ale core build")
    parser.add_argument("--rom-dir", type=Path, help="default: ale-py's bundled ROMs")
    parser.add_argument("--game", action="append", dest="games", help="default: every atari-*")
    parser.add_argument("--frames", type=int, default=20_000, help="frames per game")
    parser.add_argument("--seed", type=int, default=0, help="the actions' seed")
    parser.add_argument(
        "--reset-game",
        action="store_true",
        help="start ale-py's later episodes with ALE's reset_game() alone, as ALE v5's"
        " environments do, rather than reloading the ROM first, and chain the harness's"
        " (later_episodes 'chain'; docs/cores.md, 'ALE parity')",
    )
    args = parser.parse_args(argv)
    try:
        import ale_py
    except ImportError:
        parser.error("needs ale-py importable (docs/cores.md, 'ALE parity')")
    ale_py.ALEInterface.setLoggerMode(ale_py.LoggerMode.Error)
    rom_dir = args.rom_dir or Path(ale_py.__file__).parent / "roms"
    games = args.games or sorted(path.name for path in GAMES_DIR.glob("atari-*"))
    print(f"ale-py {ale_py.__version__}, ROMs in {rom_dir}")
    failed = False
    for game_id in games:
        with tempfile.TemporaryDirectory() as games_dir:
            result = compare(
                game_id,
                ale_py,
                rom_dir,
                args.data_dir,
                args.core_path,
                Path(games_dir),
                args.frames,
                args.seed,
                not args.reset_game,
            )
        status = "ok" if result.difference is None else "DIFFERS"
        print(
            f"{game_id}: {status}, {result.frames} frames, {result.episodes} episodes,"
            f" ALE reward sum {result.score}"
            + ("" if result.difference is None else f": {result.difference}")
        )
        failed |= result.difference is not None
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
