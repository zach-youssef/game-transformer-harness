"""``gth bench``: speed, savestate and determinism benchmarks of real cores (docs/performance.md).

A target is a core and its content. The benchmark gives it its own game config, made from
the core, the content and (for a game in ``games/``) the game's core options and
orientation: no RAM variables, the four action slots on B, A, Y and X, and a reset script
that coins and presses start a few times (`START_SCRIPT`), so play leaves the title screen
in most games. So it measures the same thing before and after a game's own config exists.

Every measurement runs in spawned processes (one core per process, docs/architecture.md),
so the calling process loads no core. Times are wall-clock `time.perf_counter_ns` around
batches of calls; the native time of a step is the time of the `_native.Core.step` call
alone, whose call boundary costs well under a microsecond (``call_us``).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import multiprocessing
import os
import platform
import queue
import statistics
import subprocess
import sys
import time
import zlib
from collections.abc import Callable
from concurrent.futures import ProcessPoolExecutor
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

import numpy as np

from gtharness import _native, cores
from gtharness.env import GAMES_DIR, RetroEnv, core_library, find_content
from gtharness.errors import ConfigError, DesyncError
from gtharness.record import Record

START_SCRIPT: list[tuple[list[str], int]] = [
    ([], 240),
    (["select"], 6),  # coin, for arcade cores
    ([], 60),
    (["start"], 6),
    ([], 120),
    (["start"], 6),
    ([], 120),
    (["start"], 6),
    ([], 240),
]
"""The benchmark's reset script: RetroPad buttons held for a number of frames."""

SLOT_BUTTONS = {"b": "shot", "a": "bomb", "y": "aux1", "x": "aux2"}


@dataclass(frozen=True)
class Target:
    """A core and its content, as the benchmark runs them.

    Args:
        name: what the results call it (a game id, or the content's file stem).
        core: the core's name in ``cores.lock``.
        core_path: the core's library.
        content_path: the content the core loads.
        system_dir: where the core looks for BIOS files.
        config_toml: the benchmark's game config (`target_config`).
    """

    name: str
    core: str
    core_path: Path
    content_path: Path
    system_dir: Path
    config_toml: str

    def config(self) -> _native.GameConfig:
        return _native.GameConfig.parse(self.config_toml, f"{self.name} (benchmark config)")


def target_config(
    core: str,
    content: Path,
    *,
    orientation: int,
    crop: tuple[int, int, int, int] | None,
    core_options: dict[str, str],
) -> str:
    """The benchmark's game config for ``core`` running ``content`` (module docstring)."""
    data = content.read_bytes()
    lines = [f"core = {json.dumps(core)}", f"orientation = {orientation}"]
    if crop is not None:
        lines.append(f"crop = [{', '.join(str(value) for value in crop)}]")
    lines += [
        "[content]",
        f"file = {json.dumps(content.name)}",
        f"size = {len(data)}",
        f'sha1 = "{hashlib.sha1(data).hexdigest()}"',
        f'crc32 = "{zlib.crc32(data):08x}"',
        "[buttons]",
        *(f'{button} = "{slot}"' for button, slot in SLOT_BUTTONS.items()),
        "[reset]",
        "script = [",
        *(
            f"  {{ press = {json.dumps(press)}, frames = {frames} }},"
            for press, frames in START_SCRIPT
        ),
        "]",
        "[episode]",
        'terminate = "0"',
        "[core_options]",
        *(
            f"{json.dumps(key)} = {json.dumps(value)}"
            for key, value in sorted(core_options.items())
        ),
    ]
    return "\n".join(lines) + "\n"


def game_target(
    game_id: str,
    data_dir: Path,
    rom_dir: str | os.PathLike[str] | None,
    games_dir: Path = GAMES_DIR,
) -> Target:
    """The core and content of ``games_dir/<game_id>/game.toml``, found as
    `RetroEnv.from_game` finds them, with the game's core options, orientation and crop."""
    game = _native.GameConfig.load(games_dir / game_id / "game.toml")
    content = find_content(game, game_id, rom_dir)
    toml = target_config(
        game.core,
        content,
        orientation=game.orientation,
        crop=game.crop,
        core_options=game.core_options,
    )
    path = _core_path(data_dir, game.core)
    return Target(game_id, game.core, path, content, data_dir / "system", toml)


def content_target(core: str, content: Path, data_dir: Path) -> Target:
    """``core`` from ``data_dir/cores`` running ``content``, which has no game config yet,
    with the core's default options and no turn or crop."""
    if not content.is_file():
        raise ConfigError(f"{content} isn't a file")
    toml = target_config(core, content, orientation=0, crop=None, core_options={})
    path = _core_path(data_dir, core)
    return Target(content.stem, core, path, content, data_dir / "system", toml)


def _core_path(data_dir: Path, core: str) -> Path:
    path = core_library(data_dir, core)
    if not path.is_file():
        raise ConfigError(f"no {path.name} in {path.parent} (gth cores fetch, docs/cores.md)")
    return path


def make_env(
    target: Target, frameskip: int, obs_size: tuple[int, int], record: Record | None = None
) -> RetroEnv:
    """``target`` as a flat-action environment with a reward of 0."""
    return RetroEnv(
        target.config(),
        core_path=target.core_path,
        content_path=target.content_path,
        system_dir=target.system_dir,
        frameskip=frameskip,
        obs_size=obs_size,
        action_format="flat",
        reward=_no_reward,
        reset_state=None,
        record=record,
        game_id=target.name if record is not None else None,
    )


def _no_reward(_previous: dict[str, int], _current: dict[str, int]) -> float:
    return 0.0


def _random_actions(env: RetroEnv, count: int, seed: int) -> list[int]:
    rng = np.random.default_rng(seed)
    return [int(action) for action in rng.integers(0, env.config.n_actions, count)]


# Measurements. Each `_*_worker` runs in a process of its own.


def _speed_worker(
    target: Target, frameskips: list[int], obs_size: tuple[int, int], steps: int, rounds: int
) -> dict[str, Any]:
    env = make_env(target, frameskips[0], obs_size)
    try:
        env.reset(seed=0)
        actions = _random_actions(env, steps, seed=1)
        table = env.config.joypad_table()
        masks = [table[action] for action in actions]
        core = env._core
        timed = _TimedCore(core)
        results: dict[str, Any] = {"system_info": env.system_info(), "frameskip": {}}
        # Every round of every frameskip starts from the state right after the reset, so the
        # rows differ by frameskip, not by how far random play has taken the game.
        state = core.serialize()
        for frameskip in frameskips:
            env.frameskip = frameskip
            runs: dict[str, list[float]] = {
                "env": [],
                "native": [],
                "emulate": [],
                "python": [],
                "call": [],
            }
            for _ in range(rounds):
                # The three loops run the same frames from the same state, so the differences
                # between them are the costs of what they add, not of different game scenes.
                core.unserialize(state)
                env._core = timed  # type: ignore[assignment]
                timed.ns = 0
                start = time.perf_counter_ns()
                for action in actions:
                    env.step(action)
                env_ns = (time.perf_counter_ns() - start) / steps
                env._core = core
                inner_ns = timed.ns / steps
                core.unserialize(state)
                start = time.perf_counter_ns()
                for mask in masks:
                    core.step(frameskip, mask)
                native_ns = (time.perf_counter_ns() - start) / steps
                core.unserialize(state)
                start = time.perf_counter_ns()
                for mask in masks:
                    core.run_inputs([(mask, frameskip)])
                runs["emulate"].append((time.perf_counter_ns() - start) / steps)
                start = time.perf_counter_ns()
                for _ in masks:
                    _ = core.frames_run
                runs["call"].append((time.perf_counter_ns() - start) / steps)
                runs["env"].append(env_ns)
                runs["native"].append(native_ns)
                runs["python"].append(env_ns - inner_ns)
            us = {name: statistics.median(values) / 1000 for name, values in runs.items()}
            results["frameskip"][frameskip] = {
                "fps": frameskip * 1e6 / us["env"],
                "env_step_us": us["env"],
                "native_step_us": us["native"],
                "emulate_us": us["emulate"],
                "render_us": us["native"] - us["emulate"],
                "python_us": us["python"],
                "python_share": us["python"] / us["env"],
                "call_us": us["call"],
            }
        return results
    finally:
        env.close()


class _TimedCore:
    """A core whose `step` adds its time to ``ns``: inside `RetroEnv.step`, the rest of the
    step's time is what the environment adds to the native step (the Python, and the
    `read_ram` call). The wrapper's own call and timer reads, a fraction of a microsecond,
    count as the environment's, so it errs towards more overhead."""

    def __init__(self, core: _native.Core) -> None:
        self._core = core
        self.ns = 0

    def step(self, frames: int, buttons: int) -> Any:
        start = time.perf_counter_ns()
        observation = self._core.step(frames, buttons)
        self.ns += time.perf_counter_ns() - start
        return observation

    def __getattr__(self, name: str) -> Any:
        return getattr(self._core, name)


def _parallel_worker(
    target: Target,
    frameskip: int,
    obs_size: tuple[int, int],
    seconds: float,
    seed: int,
    barrier: Any,
    results: Any,
) -> None:
    try:
        env = make_env(target, frameskip, obs_size)
        try:
            env.reset(seed=seed)
            actions = _random_actions(env, 4096, seed)
            for action in actions[:60]:
                env.step(action)
            barrier.wait()
            steps = 0
            start = time.perf_counter()
            end = start + seconds
            while time.perf_counter() < end:
                env.step(actions[steps % len(actions)])
                steps += 1
            results.put(("ok", steps * frameskip / (time.perf_counter() - start)))
        finally:
            env.close()
    except BaseException as error:
        barrier.abort()
        results.put(("error", f"{type(error).__name__}: {error}"))


def parallel_fps(
    target: Target, frameskip: int, obs_size: tuple[int, int], processes: int, seconds: float
) -> dict[str, Any]:
    """Frames/s of ``processes`` environments stepping at once, one per process, for
    ``seconds`` after all are ready: the sum and each process's."""
    context = multiprocessing.get_context("spawn")
    barrier = context.Barrier(processes)
    results = context.Queue()
    workers = [
        context.Process(
            target=_parallel_worker,
            args=(target, frameskip, obs_size, seconds, seed, barrier, results),
        )
        for seed in range(processes)
    ]
    for worker in workers:
        worker.start()
    outcomes = []
    try:
        for _ in workers:
            outcomes.append(results.get(timeout=seconds + 120))
    except queue.Empty:
        outcomes.append(("error", "a process neither finished nor reported an error"))
    finally:
        for worker in workers:
            worker.join(timeout=30)
            if worker.is_alive():
                worker.kill()
    errors = [detail for status, detail in outcomes if status != "ok"]
    if errors:
        raise RuntimeError(f"{target.name}: parallel run failed: {errors[0]}")
    fps = sorted(detail for _, detail in outcomes)
    return {"processes": processes, "fps": sum(fps), "per_process_fps": fps}


def _savestate_worker(
    target: Target, obs_size: tuple[int, int], repeats: int, check_steps: int
) -> dict[str, Any]:
    env = make_env(target, 1, obs_size)
    try:
        env.reset(seed=0)
        core = env._core
        state = core.serialize()
        # The live run from the saved state, before any load, then the same actions after
        # loading it: a core whose savestate misses some of its state diverges here.
        actions = _random_actions(env, check_steps, seed=2)
        runs = [[hashlib.sha1(env.step(action)[0]).digest() for action in actions]]
        save, load = [], []
        for _ in range(repeats):
            start = time.perf_counter_ns()
            core.serialize()
            save.append(time.perf_counter_ns() - start)
        for _ in range(repeats):
            start = time.perf_counter_ns()
            core.unserialize(state)
            load.append(time.perf_counter_ns() - start)
        core.unserialize(state)
        runs.append([hashlib.sha1(env.step(action)[0]).digest() for action in actions])
        differs = next((i for i, (a, b) in enumerate(zip(*runs, strict=True)) if a != b), None)
        return {
            "size": len(state),
            "save_us": statistics.median(save) / 1000,
            "load_us": statistics.median(load) / 1000,
            "reload_checked_frames": check_steps,
            "reload_first_difference": None if differs is None else differs + 1,
        }
    finally:
        env.close()


def _record_worker(
    target: Target, frames: int, frameskip: int, out_dir: Path, seed: int
) -> dict[str, Any]:
    record = Record(
        directory=out_dir,
        recorder="scripted",
        player=f"gtharness.bench random actions, seed {seed}",
        license="not-shared",
        checks_every=1,
    )
    env = make_env(target, frameskip, (84, 84), record)
    try:
        env.reset(seed=seed)
        actions = _random_actions(env, frames // frameskip, seed)
        start = time.perf_counter()
        for action in actions:
            env.step(action)
        elapsed = time.perf_counter() - start
    finally:
        env.close()
    (path,) = env.recordings
    return {"recording": str(path), "record_s": elapsed}


def replay_check(target: Target, recording: Path) -> dict[str, Any]:
    """Replays ``recording`` with ``target``'s core and content, checking every desync
    check: the frames replayed, and the desync's frame and message if there is one."""
    replay = _native.Replayer(
        recording, target.core_path, target.content_path, target.system_dir, 84, 84, None, 0, "area"
    )
    try:
        start = time.perf_counter()
        try:
            replay.run_to_end()
            desync = None
        except DesyncError as error:
            desync = {"frame": replay.frame, "error": str(error)}
        return {
            "frames": replay.frames,
            "replayed": replay.frame,
            "replay_s": time.perf_counter() - start,
            "desync": desync,
        }
    finally:
        replay.close()


def determinism(target: Target, frames: int, out_dir: Path, seed: int) -> dict[str, Any]:
    """Records ``frames`` frames of random actions (frameskip 4) into ``out_dir``, then
    replays the recording in a fresh process, so state a core keeps between loads in one
    process can't hide a difference."""
    recorded = _isolated(_record_worker, target, frames, 4, out_dir, seed)
    replayed = _isolated(replay_check, target, Path(recorded["recording"]))
    return recorded | replayed


def _isolated(function: Callable[..., Any], *args: Any) -> Any:
    """``function(*args)`` in a spawned process of its own."""
    with ProcessPoolExecutor(1, mp_context=multiprocessing.get_context("spawn")) as pool:
        return pool.submit(function, *args).result()


@dataclass(frozen=True, kw_only=True)
class Settings:
    """What to measure; every field is required, so a run's settings are in its report."""

    frameskips: list[int]
    obs_size: tuple[int, int]
    steps: int
    rounds: int
    processes: list[int]
    seconds: float
    savestate_repeats: int
    determinism_frames: int
    out_dir: Path


def default_processes() -> list[int]:
    """1, 2, 4 and one less than the CPU count, those that fit the machine."""
    count = os.cpu_count() or 1
    return sorted({n for n in (1, 2, 4, count - 1) if 1 <= n <= count})


def bench(target: Target, settings: Settings, report: Callable[[str], None]) -> dict[str, Any]:
    """Every measurement of ``target``, as a JSON-able dict."""
    report(f"{target.name}: single process")
    speed = _isolated(
        _speed_worker,
        target,
        settings.frameskips,
        settings.obs_size,
        settings.steps,
        settings.rounds,
    )
    parallel: dict[int, list[dict[str, Any]]] = {}
    for frameskip in settings.frameskips:
        for processes in settings.processes:
            report(f"{target.name}: frameskip {frameskip}, {processes} processes")
            parallel.setdefault(frameskip, []).append(
                parallel_fps(target, frameskip, settings.obs_size, processes, settings.seconds)
            )
    report(f"{target.name}: savestates")
    savestate = _isolated(
        _savestate_worker, target, settings.obs_size, settings.savestate_repeats, 600
    )
    report(f"{target.name}: determinism, {settings.determinism_frames} frames")
    out = settings.out_dir / target.name
    out.mkdir(parents=True, exist_ok=True)
    determinism_result = determinism(target, settings.determinism_frames, out, seed=0)
    lock = cores.pinned_lock(cores.LOCK, target.core, target.core_path)
    return {
        "target": target.name,
        "core": target.core,
        "content": target.content_path.name,
        "library_version": speed.pop("system_info")["library_version"],
        "pinned": lock is not None,
        "speed": speed["frameskip"],
        "parallel": parallel,
        "savestate": savestate,
        "determinism": determinism_result,
    }


def machine() -> dict[str, Any]:
    """The machine and harness build the benchmark ran on."""
    info: dict[str, Any] = {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "cpu": platform.processor(),
        "cpu_count": os.cpu_count(),
        "python": platform.python_version(),
        "harness": _native.build_info(),
    }
    if sys.platform == "darwin":
        for key, name in [
            ("cpu", "machdep.cpu.brand_string"),
            ("performance_cores", "hw.perflevel0.physicalcpu"),
            ("efficiency_cores", "hw.perflevel1.physicalcpu"),
        ]:
            value = _sysctl(name)
            if value:
                info[key] = value
        info["os"] = f"macOS {platform.mac_ver()[0]}"
    elif sys.platform == "win32":
        info["os"] = f"Windows {platform.version()}"
        name = _windows_cpu_name()
        if name:
            info["cpu"] = name
    else:
        info["os"] = platform.platform()
        try:
            for line in Path("/proc/cpuinfo").read_text().splitlines():
                if line.startswith("model name"):
                    info["cpu"] = line.split(":", 1)[1].strip()
                    break
        except OSError:
            pass
    return info


def _windows_cpu_name() -> str | None:
    """The chip name (`platform.processor()` gives only the family and model numbers)."""
    if sys.platform != "win32":
        return None
    import winreg

    key = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"
    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, key) as handle:
            value, _ = winreg.QueryValueEx(handle, "ProcessorNameString")
    except OSError:
        return None
    return str(value).strip() or None


def _sysctl(name: str) -> str | None:
    try:
        result = subprocess.run(["sysctl", "-n", name], capture_output=True, text=True, check=True)
    except (OSError, subprocess.CalledProcessError):
        return None
    return result.stdout.strip() or None


def markdown(run: dict[str, Any]) -> str:
    """The tables of docs/performance.md for one run of `gth bench`."""
    m = run["machine"]
    s = run["settings"]
    lines = [
        f"Machine: {m['cpu']}, {m['cpu_count']} CPUs"
        + (
            f" ({m['performance_cores']} performance, {m['efficiency_cores']} efficiency)"
            if "performance_cores" in m and "efficiency_cores" in m
            else ""
        )
        + f", {m['os']}; harness {m['harness']['git_sha'][:7]} ({m['harness']['profile']}).",
        "",
        "| Target | Core | Version | Pinned build |",
        "|---|---|---|---|",
    ]
    for t in run["targets"]:
        lines.append(
            f"| {t['target']} | `{t['core']}` | `{t['library_version']}` | {_yes(t['pinned'])} |"
        )
    obs = "x".join(str(v) for v in s["obs_size"])
    lines += [
        "",
        f"Single process, {obs} observations: frames/s and the time of one step in µs.",
        "",
        "| Target | Frameskip | Frames/s | Env step | Native step | Emulate | Render | Python"
        " | Python share |",
        "|---|---|---|---|---|---|---|---|---|",
    ]
    for t in run["targets"]:
        for fs, r in t["speed"].items():
            lines.append(
                f"| {t['target']} | {fs} | {r['fps']:.0f} | {r['env_step_us']:.0f} |"
                f" {r['native_step_us']:.0f} | {r['emulate_us']:.0f} | {r['render_us']:.0f} |"
                f" {r['python_us']:.1f} | {r['python_share']:.1%} |"
            )
    procs = s["processes"]
    lines += [
        "",
        f"Processes at once, {obs} observations: total frames/s (and per process).",
        "",
        "| Target | Frameskip | " + " | ".join(f"N = {n}" for n in procs) + " |",
        "|---|---|" + "---|" * len(procs),
    ]
    for t in run["targets"]:
        for fs, runs in t["parallel"].items():
            cells = [f"{r['fps']:.0f} ({r['fps'] / r['processes']:.0f})" for r in runs]
            lines.append(f"| {t['target']} | {fs} | " + " | ".join(cells) + " |")
    lines += [
        "",
        "| Target | Savestate bytes | Save µs | Load µs | Reload matches |",
        "|---|---|---|---|---|",
    ]
    for t in run["targets"]:
        r = t["savestate"]
        diff = r["reload_first_difference"]
        same = f"yes, {r['reload_checked_frames']} frames" if diff is None else f"no: frame {diff}"
        lines.append(
            f"| {t['target']} | {r['size']} | {r['save_us']:.0f} | {r['load_us']:.0f} | {same} |"
        )
    lines += [
        "",
        "| Target | Frames recorded | Replayed | Desync | Record s | Replay s |",
        "|---|---|---|---|---|---|",
    ]
    for t in run["targets"]:
        r = t["determinism"]
        desync = "none" if r["desync"] is None else f"frame {r['desync']['frame']}"
        lines.append(
            f"| {t['target']} | {r['frames']} | {r['replayed']} | {desync} |"
            f" {r['record_s']:.1f} | {r['replay_s']:.1f} |"
        )
    return "\n".join(lines)


def _yes(value: bool) -> str:
    return "yes" if value else "no"


def add_parser(commands: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    """Adds ``gth bench`` to the ``gth`` command line."""
    parser = commands.add_parser(
        "bench",
        help="measure speed, savestates and determinism on real cores",
        description="Benchmarks each target (docs/performance.md) and prints the tables of "
        "docs/performance.md. Exit status: 0 when every replay matched, 1 on a desync or a "
        "reloaded state that diverged, 2 on a usage or config error.",
    )
    parser.add_argument("data_dir", type=Path, help="data directory, as for RetroEnv.from_game")
    parser.add_argument("--game", action="append", default=[], help="a game in games/ (repeatable)")
    parser.add_argument(
        "--content",
        action="append",
        default=[],
        metavar="CORE=PATH",
        help="content with no game config yet, on core CORE (repeatable)",
    )
    parser.add_argument(
        "--out", type=Path, required=True, help="directory for the determinism recordings"
    )
    parser.add_argument("--frameskip", type=int, nargs="+", default=[1, 2, 4])
    parser.add_argument("--obs-size", type=int, nargs=2, default=[256, 256])
    parser.add_argument("--steps", type=int, default=1000, help="steps per timed batch")
    parser.add_argument("--rounds", type=int, default=5, help="timed batches (median)")
    parser.add_argument(
        "--processes",
        type=int,
        nargs="+",
        default=None,
        help="process counts (default: 1, 2, 4 and the CPU count minus one)",
    )
    parser.add_argument("--seconds", type=float, default=5.0, help="per parallel run")
    parser.add_argument("--savestate-repeats", type=int, default=100)
    parser.add_argument("--determinism-frames", type=int, default=36000, help="default: 10 minutes")
    parser.add_argument(
        "--rom-dir", type=Path, help="default: GTH_ROM_DIR or rom_dir in the user config"
    )
    parser.add_argument("--games-dir", type=Path, default=GAMES_DIR)
    parser.add_argument("--json", type=Path, help="also write the results as JSON here")


def main(args: argparse.Namespace) -> int:
    def report(line: str) -> None:
        print(line, file=sys.stderr, flush=True)

    try:
        targets = [game_target(g, args.data_dir, args.rom_dir, args.games_dir) for g in args.game]
        for spec in args.content:
            core, sep, path = spec.partition("=")
            if not sep or not core or not path:
                raise ConfigError(f"--content takes CORE=PATH, not {spec!r}")
            targets.append(content_target(core, Path(path), args.data_dir))
        if not targets:
            raise ConfigError("nothing to benchmark: give --game or --content")
        counts = [*args.frameskip, *(args.processes or [])]
        counts += [args.steps, args.rounds, args.savestate_repeats]
        if any(n < 1 for n in counts) or min(args.obs_size) < 1:
            raise ConfigError(
                "--frameskip, --processes, --steps, --rounds, --savestate-repeats and "
                "--obs-size take integers >= 1"
            )
        if not args.seconds > 0:
            raise ConfigError("--seconds takes a time > 0")
        if args.determinism_frames < 4 or args.determinism_frames % 4:
            raise ConfigError("--determinism-frames takes a positive multiple of 4 (frameskip 4)")
        settings = Settings(
            frameskips=args.frameskip,
            obs_size=(args.obs_size[0], args.obs_size[1]),
            steps=args.steps,
            rounds=args.rounds,
            processes=args.processes or default_processes(),
            seconds=args.seconds,
            savestate_repeats=args.savestate_repeats,
            determinism_frames=args.determinism_frames,
            out_dir=args.out,
        )
    except ConfigError as error:
        print(f"gth bench: {error}", file=sys.stderr)
        return 2
    info = machine()
    if info["harness"]["profile"] != "release":
        report("gth bench: warning: a debug build of _native (maturin develop --release)")
    run = {
        "machine": info,
        "settings": asdict(settings) | {"out_dir": str(settings.out_dir)},
        "targets": [bench(target, settings, report) for target in targets],
    }
    if args.json:
        args.json.write_text(json.dumps(run, indent=2) + "\n", encoding="utf-8")
    print(markdown(run))
    failed = [
        t["target"]
        for t in run["targets"]
        if t["determinism"]["desync"] is not None
        or t["savestate"]["reload_first_difference"] is not None
    ]
    return 1 if failed else 0
