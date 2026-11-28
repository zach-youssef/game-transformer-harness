"""The evaluation runner (docs/evaluation.md, "Running an evaluation"): `evaluate` and
``gth evaluate`` play a policy under a protocol and write per-episode results with the
protocol attached.

An evaluation plays every game of the protocol, for every pair of policy seed and env seed,
``episodes_per_seed`` episodes. Each pair runs in a spawned process of its own (one core per
process, docs/architecture.md), ``jobs`` at a time; the results don't depend on ``jobs``. The
output directory gets ``protocol.toml`` (the expanded protocol), ``episodes.jsonl`` (one row
per episode) and, last, ``run.json`` (the policy, harness build, machine and timings), whose
presence marks a finished run. `check_results` validates a directory against the documented
schema.
"""

from __future__ import annotations

import argparse
import importlib
import json
import multiprocessing
import multiprocessing.connection
import os
import re
import sys
import time
import tomllib
from collections import deque
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np

from gtharness import _native, bench
from gtharness import protocol as protocol_module
from gtharness.env import GAMES_DIR, RetroEnv, core_library, find_content
from gtharness.errors import ConfigError, HarnessError
from gtharness.policies import GameInfo, Policy, check_identity
from gtharness.protocol import EvalProtocol
from gtharness.record import Record

__all__ = [
    "ATARI_ACTIONS",
    "RESULTS_FORMAT",
    "EvaluationFailed",
    "Recording",
    "check_protocol",
    "check_results",
    "evaluate",
    "make_policy",
]

_SHA256 = re.compile(r"[0-9a-f]{64}")

RESULTS_FORMAT = 1
"""``run.json``'s ``format``: bumped whenever a results file's fields change."""

ATARI_ACTIONS = 18
"""ALE's full action set: under ``atari_full`` a policy acts in ``0..18`` (docs/actions.md,
"Atari")."""

EPISODE_FIELDS = (
    "game",
    "env_seed",
    "policy_seed",
    "policy_id",
    "checkpoint_sha256",
    "episode",
    "score",
    "frames",
    "steps",
    "terminated",
    "truncated",
    "mapped_slots_used",
    "recording",
)
"""An ``episodes.jsonl`` row's fields, in the order written (docs/evaluation.md, "Results")."""


class EvaluationFailed(HarnessError):
    """An episode's process failed (a core error, a policy error or a crash): the run is
    incomplete and has no ``run.json``."""


@dataclass(frozen=True, kw_only=True)
class Recording:
    """Recording every episode (recorder ``policy``); both fields are as in `Record`."""

    license: str
    checks_every: int


def make_policy(spec: str, kwargs: Mapping[str, Any], policy_seed: int) -> Policy:
    """The policy ``MODULE:NAME`` names for ``policy_seed``: ``NAME(policy_seed=policy_seed,
    **kwargs)``, checked by `check_identity`. Raises ConfigError when the module or name
    doesn't exist, making the policy raises, or the result isn't a Policy."""
    module_name, _, name = spec.partition(":")
    if not module_name or not name:
        raise ConfigError(f"--policy must be MODULE:NAME, not {spec!r}")
    try:
        module = importlib.import_module(module_name)
    except ImportError as error:
        raise ConfigError(f"{spec}: cannot import {module_name}: {error}") from error
    factory = getattr(module, name, None)
    if not callable(factory):
        raise ConfigError(f"{spec}: {module_name} has no callable {name}")
    if "policy_seed" in kwargs:
        raise ConfigError(f"{spec}: policy_seed comes from the protocol, not the policy kwargs")
    try:
        made = factory(policy_seed=policy_seed, **kwargs)
    except Exception as error:  # a missing checkpoint, a bad kwarg, the model library's error
        raise ConfigError(
            f"{spec}: making the policy for policy seed {policy_seed} failed:"
            f" {type(error).__name__}: {error}"
        ) from error
    try:
        return check_identity(made, spec)
    except TypeError as error:
        raise ConfigError(f"{spec}: {error}") from error


def check_protocol(
    protocol: EvalProtocol,
    data_dir: str | os.PathLike[str],
    *,
    rom_dir: str | os.PathLike[str] | None,
    core_path: str | os.PathLike[str] | None,
    games_dir: str | os.PathLike[str],
) -> dict[str, dict[str, str]]:
    """The checks of a protocol that need the games and the env (docs/evaluation.md, "Checks
    before a run"); returns each game's core and content SHA-1. Raises ConfigError naming
    the game and what fails."""
    if protocol.start.reset != "game":
        raise ConfigError(
            'start.reset: the runner starts episodes only from the game\'s reset ("game"),'
            " not from another savestate"
        )
    cap, noops, skip = protocol.max_episode_frames, protocol.start.noop_max, protocol.frameskip
    if noops + skip >= cap:
        raise ConfigError(
            f"start.noop_max + frameskip ({noops} + {skip}) must be below max_episode_frames"
            f" ({cap}), so the reset leaves an episode at least one step"
        )
    games: dict[str, dict[str, str]] = {}
    data = Path(data_dir)
    for game_id in protocol.games:
        path = Path(games_dir) / game_id / "game.toml"
        if not path.is_file():
            raise ConfigError(f"{game_id}: no game config {path}")
        config = _native.GameConfig.load(path)
        if protocol.score_var not in config.ram_names:
            raise ConfigError(
                f"{game_id}: score_var {protocol.score_var!r} isn't one of its RAM variables"
                f" {config.ram_names}"
            )
        if protocol.action_set == "atari_minimal" and not config.atari_minimal_actions:
            raise ConfigError(
                f"{game_id}: action_set 'atari_minimal' needs the game's minimal action set"
                " (atari_minimal_actions in its game.toml), and it has none: not an Atari game"
            )
        for field, wanted, own in (
            ("obs.crop", protocol.obs.crop, config.crop),
            ("obs.orientation", protocol.obs.orientation, config.orientation),
        ):
            if wanted != "game" and _tupled(wanted) != _tupled(own):
                raise ConfigError(
                    f"{game_id}: {field} {wanted!r} differs from the game config's {own!r}, and"
                    ' the env uses the game\'s; use "game"'
                )
        # Found by SHA-1, so this pins the dump; BIOS files and the core are checked too, so
        # no worker fails on a file that was never there.
        find_content(config, game_id, rom_dir)
        missing = config.missing_bios(data / "system")
        if missing:
            raise ConfigError(f"{game_id}: BIOS files missing from {data / 'system'}: {missing}")
        if config.reset_state is not None:
            state = data / "states" / game_id / config.reset_state
            if not state.is_file():
                raise ConfigError(f"{game_id}: no reset state {state}")
        library = Path(core_path) if core_path else core_library(data, config.core)
        if not library.is_file():
            raise ConfigError(f"{game_id}: no core library {library} (gth cores fetch)")
        content = tomllib.loads(config.content_toml)["content"]
        games[game_id] = {"core": config.core, "content_sha1": content["sha1"]}
    if core_path is not None and len({game["core"] for game in games.values()}) > 1:
        raise ConfigError("--core-path gives one core, and the protocol's games use several")
    return games


def evaluate(
    protocol: EvalProtocol,
    policy: str,
    out_dir: str | os.PathLike[str],
    data_dir: str | os.PathLike[str],
    *,
    policy_kwargs: Mapping[str, Any],
    record: Recording | None,
    jobs: int,
    rom_dir: str | os.PathLike[str] | None = None,
    core_path: str | os.PathLike[str] | None = None,
    games_dir: str | os.PathLike[str] = GAMES_DIR,
    report: Callable[[str], None] | None = None,
) -> Path:
    """Plays ``policy`` (``MODULE:NAME``, made with ``policy_kwargs``) under ``protocol``
    and writes the results to ``out_dir`` (new or empty), returning its ``run.json``.

    Everything is checked before the first episode: the protocol against the games and the
    env (`check_protocol`) and the policy's `ObsRequirements` against the protocol. Files
    are found as `RetroEnv.from_game` finds them. With ``record``, every episode is recorded
    under ``out_dir/recordings/<game>/``. ``report`` gets a line per finished pair of seeds.
    Raises ConfigError for a refused run (nothing written) and `EvaluationFailed` when an
    episode's process fails (no ``run.json``)."""
    if jobs < 1:
        raise ConfigError(f"jobs must be at least 1, not {jobs}")
    if record is not None and record.checks_every < 1:
        raise ConfigError(f"checks_every must be at least 1, not {record.checks_every}")
    out = Path(out_dir)
    if out.exists() and (not out.is_dir() or any(out.iterdir())):
        raise ConfigError(f"{out} isn't a new or empty directory: results are never overwritten")
    kwargs = dict(policy_kwargs)
    try:
        json.dumps(kwargs)
    except TypeError as error:
        raise ConfigError(f"policy kwargs must be JSON values: {error}") from error
    games = check_protocol(
        protocol, data_dir, rom_dir=rom_dir, core_path=core_path, games_dir=games_dir
    )
    expanded = protocol.expand()
    # In a process of its own, so a model policy's weights (and GPU context) don't stay
    # loaded in this one for the whole run.
    try:
        (checked,) = _run_all(
            _check_policies, [(policy, kwargs, protocol.policy_seeds, expanded.text)], 1
        )
    except EvaluationFailed as error:
        raise ConfigError(f"{policy}: checking the policy failed: {error}") from error
    if isinstance(checked, str):
        raise ConfigError(checked)
    identities: dict[int, tuple[str, str | None]] = checked

    out.mkdir(parents=True, exist_ok=True)
    (out / "protocol.toml").write_text(expanded.text, encoding="utf-8")
    tasks = [
        {
            "game_id": game_id,
            "policy_seed": policy_seed,
            "env_seed": env_seed,
            "protocol": expanded.text,
            "policy": policy,
            "policy_kwargs": kwargs,
            "identity": identities[policy_seed],
            "out_dir": str(out),
            "record": None if record is None else (record.license, record.checks_every),
            "data_dir": str(data_dir),
            "rom_dir": None if rom_dir is None else str(rom_dir),
            "core_path": None if core_path is None else str(core_path),
            "games_dir": str(games_dir),
        }
        for game_id in protocol.games
        for policy_seed in protocol.policy_seeds
        for env_seed in protocol.env_seeds
    ]
    started = time.time()
    start = time.perf_counter()

    def done(task: dict[str, Any], result: dict[str, Any]) -> None:
        if report is not None:
            report(f"{_pair(task)}: scores {[row['score'] for row in result['episodes']]}")

    results = _run_all(_play, tasks, jobs, describe=_pair, done=done)
    wall = time.perf_counter() - start

    with (out / "episodes.jsonl").open("w", encoding="utf-8") as file:
        for result in results:
            for row in result["episodes"]:
                file.write(json.dumps(row) + "\n")
    steps = sum(result["steps"] for result in results)
    act_ns = sum(result["act_ns"] for result in results)
    machine = bench.machine()
    harness = machine.pop("harness")
    run_info = {
        "format": RESULTS_FORMAT,
        "protocol_sha1": expanded.sha1,
        "policy": {
            "spec": policy,
            "kwargs": kwargs,
            "seeds": {
                str(seed): {"id": policy_id, "checkpoint_sha256": checkpoint}
                for seed, (policy_id, checkpoint) in identities.items()
            },
        },
        "games": games,
        "recorded": record is not None,
        "harness": harness,
        "machine": machine,
        "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(started)),
        "wall_s": wall,
        "jobs": jobs,
        "episodes": sum(len(result["episodes"]) for result in results),
        "steps": steps,
        "policy_calls": sum(result["calls"] for result in results),
        "policy_ms_per_step": act_ns / 1e6 / steps,
    }
    path = out / "run.json"
    path.write_text(json.dumps(run_info, indent=2) + "\n", encoding="utf-8")
    return path


def _pair(task: dict[str, Any]) -> str:
    return f"{task['game_id']}, policy seed {task['policy_seed']}, env seed {task['env_seed']}"


def _run_all(
    function: Callable[[Any], Any],
    arguments: Sequence[Any],
    jobs: int,
    *,
    describe: Callable[[Any], str] = lambda _: "",
    done: Callable[[Any, Any], None] = lambda _a, _r: None,
) -> list[Any]:
    """``[function(argument) for argument in arguments]``, each call in a spawned process of
    its own, ``jobs`` at a time, in order; ``done`` gets each result as it arrives.

    The first failure (an exception, or the process dying) raises `EvaluationFailed` naming
    ``describe(argument)`` at once: no further call starts and the running ones are
    terminated, so a broken policy or game is reported in seconds, not after the run."""
    context = multiprocessing.get_context("spawn")
    results: list[Any] = [None] * len(arguments)
    queue = deque(enumerate(arguments))
    running: dict[Any, tuple[int, Any]] = {}
    try:
        while queue or running:
            while queue and len(running) < jobs:
                index, argument = queue.popleft()
                receive, send = context.Pipe(duplex=False)
                process = context.Process(target=_child, args=(send, function, argument))
                process.start()
                send.close()  # so a dead child reads as end of file
                running[receive] = (index, process)
            for receive in multiprocessing.connection.wait(list(running)):
                index, process = running.pop(receive)
                try:
                    status, value = receive.recv()
                except EOFError:
                    process.join()
                    status, value = "error", f"its process exited with code {process.exitcode}"
                receive.close()
                process.join()
                prefix = describe(arguments[index])
                if status != "ok":
                    raise EvaluationFailed(f"{prefix}: {value}" if prefix else value)
                results[index] = value
                done(arguments[index], value)
    finally:
        for receive, (_, process) in running.items():
            process.terminate()
            process.join()
            receive.close()
    return results


def _child(send: Any, function: Callable[[Any], Any], argument: Any) -> None:
    try:
        message = ("ok", function(argument))
    except BaseException as error:  # reported, then the process ends
        message = ("error", f"{type(error).__name__}: {error}")
    send.send(message)
    send.close()


def _check_policies(
    argument: tuple[str, dict[str, Any], Sequence[int], str],
) -> dict[int, tuple[str, str | None]] | str:
    """Makes the policy for every policy seed and checks it against the protocol: each seed's
    ``(id, checkpoint_sha256)``, or the ConfigError's message."""
    spec, kwargs, seeds, text = argument
    protocol = protocol_module.loads(text)
    identities = {}
    try:
        for seed in seeds:
            made = make_policy(spec, kwargs, seed)
            mismatches = made.obs_requirements.mismatches(protocol)
            if mismatches:
                return f"{spec} can't play this protocol: {'; '.join(mismatches)}"
            identities[seed] = (made.id, made.checkpoint_sha256)
            del made
    except ConfigError as error:
        return str(error)
    return identities


def _no_reward(previous: Mapping[str, int], current: Mapping[str, int]) -> float:
    """`RetroEnv` needs a reward; the runner scores an episode from RAM at its ends."""
    del previous, current
    return 0.0


def _episode_seed(policy_seed: int, env_seed: int) -> int:
    """The seed of `Policy.reset` at a pair's first episode: derived from both seeds, so a
    stochastic policy plays different action streams under different env seeds."""
    return int(np.random.SeedSequence([policy_seed, env_seed]).generate_state(1)[0])


def _play(task: dict[str, Any]) -> dict[str, Any]:
    """One pair of seeds' episodes, in this (worker) process: the rows of episodes.jsonl
    and the policy's time."""
    protocol = protocol_module.loads(task["protocol"])
    game_id, policy_seed, env_seed = task["game_id"], task["policy_seed"], task["env_seed"]
    policy = make_policy(task["policy"], task["policy_kwargs"], policy_seed)
    if (policy.id, policy.checkpoint_sha256) != tuple(task["identity"]):
        raise ConfigError(
            f"{task['policy']} made a different policy in a worker ({policy.id},"
            f" {policy.checkpoint_sha256}) than when checked ({task['identity']})"
        )
    games_dir = Path(task["games_dir"])
    out = Path(task["out_dir"])
    record = None
    if task["record"] is not None:
        license, checks_every = task["record"]
        checkpoint = policy.checkpoint_sha256 or "none"
        record = Record(
            directory=out / "recordings" / game_id,
            recorder="policy",
            player=f"{policy.id} checkpoint={checkpoint} seed={policy_seed}",
            license=license,
            checks_every=checks_every,
        )
    env = RetroEnv.from_game(
        game_id,
        task["data_dir"],
        frameskip=protocol.frameskip,
        obs_size=protocol.obs.size,
        action_format="flat",
        reward=_no_reward,
        sticky_prob=protocol.sticky_prob,
        max_episode_frames=protocol.max_episode_frames,
        noop_max=protocol.start.noop_max,
        rom_dir=task["rom_dir"],
        core_path=task["core_path"],
        games_dir=games_dir,
        interpolation=protocol.obs.interpolation,
        maxpool=protocol.maxpool,
        grayscale=protocol.obs.grayscale,
        record=record,
    )
    config = env.config
    # info["episode_frame"] counts the reset script's frames, which the cap doesn't.
    script_frames = sum(frames for _, frames in config.reset_inputs or [])
    # The policy's actions: flat ones, or ALE's numbers mapped onto the pad (docs/actions.md).
    if protocol.action_set == "atari_full":
        n_actions, to_flat = ATARI_ACTIONS, config.from_atari
    elif protocol.action_set == "atari_minimal":
        minimal = config.atari_minimal_actions
        n_actions = len(minimal)
        flat = [config.from_atari(atari) for atari in minimal]
        to_flat = flat.__getitem__
    else:
        n_actions, to_flat = config.n_actions, int
    game = GameInfo(game_id, n_actions, tuple(config.mapped_slots))
    rows: list[dict[str, Any]] = []
    act_ns = calls = steps_total = 0
    try:
        for episode in range(protocol.episodes_per_seed):
            first = episode == 0
            # Seeded once per pair; both RNGs run on across the pair's episodes.
            obs, info = env.reset(seed=env_seed if first else None)
            policy.reset(_episode_seed(policy_seed, env_seed) if first else None, game)
            stack = deque([obs] * protocol.frame_stack, maxlen=protocol.frame_stack)
            start_score = info["ram"][protocol.score_var]
            pending: deque[int] = deque()
            pressed = steps = 0
            terminated = truncated = False
            while not (terminated or truncated):
                if not pending:
                    begin = time.perf_counter_ns()
                    chosen = policy.act(np.stack(stack))
                    act_ns += time.perf_counter_ns() - begin
                    calls += 1
                    pending.extend(_actions(chosen, game.n_actions, task["policy"]))
                action = to_flat(pending.popleft())
                pressed |= config.decode(action)[1]
                obs, _, terminated, truncated, info = env.step(action)
                stack.append(obs)
                steps += 1
            steps_total += steps
            rows.append(
                {
                    "game": game_id,
                    "env_seed": env_seed,
                    "policy_seed": policy_seed,
                    "policy_id": policy.id,
                    "checkpoint_sha256": policy.checkpoint_sha256,
                    "episode": episode,
                    "score": info["ram"][protocol.score_var] - start_score,
                    "frames": info["episode_frame"]
                    - (script_frames if config.reset_state is None else 0),
                    "steps": steps,
                    "terminated": bool(terminated),
                    "truncated": bool(truncated),
                    "mapped_slots_used": [
                        slot
                        for bit, slot in enumerate(config.slots)
                        if pressed >> bit & 1 and slot in game.mapped_slots
                    ],
                    "recording": None,
                }
            )
    finally:
        env.close()
    if record is not None:
        for row, path in zip(rows, env.recordings, strict=True):
            row["recording"] = Path(path).relative_to(out).as_posix()
    return {"episodes": rows, "act_ns": act_ns, "calls": calls, "steps": steps_total}


def _actions(chosen: Any, n_actions: int, spec: str) -> list[int]:
    """`Policy.act`'s result as a list of actions in ``0..n_actions``; ValueError if it
    isn't one."""
    if isinstance(chosen, int | np.integer) and not isinstance(chosen, bool):
        actions = [int(chosen)]
    elif isinstance(chosen, Sequence | np.ndarray) and not isinstance(chosen, str | bytes):
        array = np.asarray(chosen)
        if array.ndim != 1 or array.size == 0 or array.dtype.kind not in "iu":
            raise ValueError(
                f"{spec} returned {chosen!r}: a chunk is a non-empty 1-D sequence of integers"
            )
        actions = [int(action) for action in array]
    else:
        raise ValueError(f"{spec} returned {chosen!r}, not a flat action or a chunk of them")
    for action in actions:
        if not 0 <= action < n_actions:
            raise ValueError(f"{spec} returned action {action}, outside 0..{n_actions}")
    return actions


def _tupled(value: object) -> object:
    return tuple(value) if isinstance(value, list | tuple) else value


def check_results(out_dir: str | os.PathLike[str]) -> dict[str, Any]:
    """Validates a finished run's directory against the schema of docs/evaluation.md
    ("Results") and returns ``{"protocol": EvalProtocol, "run": dict, "episodes": list}``.
    Raises ConfigError naming the file and what is wrong."""
    out = Path(out_dir)
    try:
        text = (out / "protocol.toml").read_text(encoding="utf-8")
        run = json.loads((out / "run.json").read_text(encoding="utf-8"))
        lines = (out / "episodes.jsonl").read_text(encoding="utf-8").splitlines()
    except (OSError, json.JSONDecodeError) as error:
        raise ConfigError(f"{out}: not a finished evaluation: {error}") from error
    protocol = protocol_module.loads(text, source=str(out / "protocol.toml"))
    expanded = protocol.expand()
    if expanded.text != text:
        raise ConfigError(f"{out}/protocol.toml: not the protocol's canonical text")
    _check_run(run, expanded.sha1, protocol, f"{out}/run.json")
    episodes = [
        _check_row(line, protocol, f"{out}/episodes.jsonl:{i + 1}", out, run)
        for i, line in enumerate(lines)
    ]
    keys = [(row["game"], row["policy_seed"], row["env_seed"], row["episode"]) for row in episodes]
    expected = [
        (game, policy_seed, env_seed, episode)
        for game in protocol.games
        for policy_seed in protocol.policy_seeds
        for env_seed in protocol.env_seeds
        for episode in range(protocol.episodes_per_seed)
    ]
    if keys != expected:
        raise ConfigError(
            f"{out}/episodes.jsonl: {len(keys)} rows, not the protocol's {len(expected)}"
            " episodes, one each, in order"
        )
    if run["episodes"] != len(episodes) or run["steps"] != sum(r["steps"] for r in episodes):
        raise ConfigError(f"{out}/run.json: episodes or steps don't match episodes.jsonl")
    return {"protocol": protocol, "run": run, "episodes": episodes}


def _need(condition: bool, where: str, what: str) -> None:
    if not condition:
        raise ConfigError(f"{where}: {what}")


def _is_int(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _check_run(run: Any, sha1: str, protocol: EvalProtocol, where: str) -> None:
    fields = {
        "format",
        "protocol_sha1",
        "policy",
        "games",
        "recorded",
        "harness",
        "machine",
        "started",
        "wall_s",
        "jobs",
        "episodes",
        "steps",
        "policy_calls",
        "policy_ms_per_step",
    }
    _need(isinstance(run, dict) and set(run) == fields, where, f"fields must be {sorted(fields)}")
    _need(run["format"] == RESULTS_FORMAT, where, f"format must be {RESULTS_FORMAT}")
    _need(run["protocol_sha1"] == sha1, where, "protocol_sha1 isn't protocol.toml's SHA-1")
    policy = run["policy"]
    _need(
        isinstance(policy, dict) and set(policy) == {"spec", "kwargs", "seeds"},
        where,
        "policy must have spec, kwargs and seeds",
    )
    _need(isinstance(policy["kwargs"], dict), where, "policy.kwargs must be an object")
    seeds = policy["seeds"]
    _need(
        isinstance(seeds, dict) and list(seeds) == [str(s) for s in protocol.policy_seeds],
        where,
        "policy.seeds must list the protocol's policy seeds",
    )
    for identity in seeds.values():
        _need(
            isinstance(identity, dict) and set(identity) == {"id", "checkpoint_sha256"},
            where,
            "a policy seed has id and checkpoint_sha256",
        )
        _need(isinstance(identity["id"], str) and bool(identity["id"]), where, "an id is empty")
        checkpoint = identity["checkpoint_sha256"]
        _need(
            checkpoint is None
            or (isinstance(checkpoint, str) and _SHA256.fullmatch(checkpoint) is not None),
            where,
            "checkpoint_sha256 must be null or 64 lowercase hex digits",
        )
    _need(
        isinstance(run["games"], dict) and list(run["games"]) == list(protocol.games),
        where,
        "games must list the protocol's games",
    )
    for game in run["games"].values():
        _need(set(game) == {"core", "content_sha1"}, where, "a game has core and content_sha1")
    _need(isinstance(run["recorded"], bool), where, "recorded must be true or false")
    _need(
        isinstance(run["harness"], dict) and "git_sha" in run["harness"],
        where,
        "harness must be the build info",
    )
    for name in ("episodes", "steps", "policy_calls", "jobs"):
        _need(_is_int(run[name]) and run[name] >= 1, where, f"{name} must be an integer >= 1")
    for name in ("wall_s", "policy_ms_per_step"):
        value = run[name]
        _need(isinstance(value, int | float) and value >= 0, where, f"{name} must be >= 0")


def _check_row(
    line: str, protocol: EvalProtocol, where: str, out: Path, run: dict[str, Any]
) -> dict[str, Any]:
    try:
        row = json.loads(line)
    except json.JSONDecodeError as error:
        raise ConfigError(f"{where}: not JSON: {error}") from error
    _need(
        isinstance(row, dict) and list(row) == list(EPISODE_FIELDS),
        where,
        f"fields must be {list(EPISODE_FIELDS)}",
    )
    _need(row["game"] in protocol.games, where, f"game {row['game']!r} isn't the protocol's")
    _need(row["env_seed"] in protocol.env_seeds, where, "env_seed isn't one of the protocol's")
    _need(row["policy_seed"] in protocol.policy_seeds, where, "policy_seed isn't the protocol's")
    identity = run["policy"]["seeds"][str(row["policy_seed"])]
    _need(
        [row["policy_id"], row["checkpoint_sha256"]]
        == [identity["id"], identity["checkpoint_sha256"]],
        where,
        "policy_id and checkpoint_sha256 must be run.json's for its policy seed",
    )
    for name in ("episode", "score", "frames", "steps"):
        _need(_is_int(row[name]), where, f"{name} must be an integer")
    _need(0 <= row["episode"] < protocol.episodes_per_seed, where, "episode is out of range")
    _need(row["steps"] >= 1, where, "steps must be at least 1")
    _need(
        protocol.frameskip < row["frames"] <= protocol.max_episode_frames,
        where,
        "frames must be above frameskip and at most max_episode_frames",
    )
    for name in ("terminated", "truncated"):
        _need(isinstance(row[name], bool), where, f"{name} must be true or false")
    _need(row["terminated"] or row["truncated"], where, "an episode ends terminated or truncated")
    _need(
        row["truncated"] == (row["frames"] == protocol.max_episode_frames),
        where,
        "truncated exactly when frames reach max_episode_frames",
    )
    slots = row["mapped_slots_used"]
    _need(
        isinstance(slots, list) and set(slots) <= {"shot", "bomb", "aux1", "aux2"},
        where,
        "mapped_slots_used must list slot names",
    )
    recording = row["recording"]
    if run["recorded"]:
        _need(
            isinstance(recording, str)
            and not Path(recording).is_absolute()
            and ".." not in Path(recording).parts
            and (out / recording / "meta.toml").is_file(),
            where,
            "recording must name a finished recording in the results directory",
        )
    else:
        _need(recording is None, where, "recording must be null in a run without --record")
    return row


def add_parser(commands: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    """Adds ``gth evaluate`` to the ``gth`` command line."""
    parser = commands.add_parser(
        "evaluate",
        help="play a policy under an evaluation protocol and write per-episode results",
        description="Plays POLICY under PROTOCOL and writes protocol.toml, episodes.jsonl and "
        "run.json to OUT (docs/evaluation.md, 'Running an evaluation'). Exit status: 0 when "
        "the run finished, 1 when an episode failed, 2 on a usage or config error.",
    )
    parser.add_argument("data_dir", type=Path, help="data directory, as for RetroEnv.from_game")
    parser.add_argument(
        "--protocol",
        required=True,
        help="a protocol file, or a preset's name in protocols/ (e.g. ours-v1)",
    )
    parser.add_argument(
        "--policy", required=True, help="MODULE:NAME, e.g. gtharness.policies.random:RandomPolicy"
    )
    parser.add_argument(
        "--policy-kwargs",
        type=json.loads,
        default={},
        help="a JSON object of keyword arguments for NAME (default: none)",
    )
    parser.add_argument("--out", type=Path, required=True, help="new or empty results directory")
    parser.add_argument(
        "--record", action="store_true", help="record every episode (recorder 'policy')"
    )
    parser.add_argument("--license", help="the recordings' license, e.g. CC-BY-NC-4.0")
    parser.add_argument("--checks-every", type=int, help="frames between desync checks")
    parser.add_argument(
        "--jobs",
        type=int,
        required=True,
        help="pairs of seeds played at once, each in a process that makes its own policy (so"
        " a model policy holds --jobs copies of its weights)",
    )
    parser.add_argument(
        "--rom-dir", type=Path, help="default: GTH_ROM_DIR or rom_dir in the user config"
    )
    parser.add_argument("--core-path", type=Path, help="default: data_dir/cores/")
    parser.add_argument("--games-dir", type=Path, default=GAMES_DIR)


def protocol_path(name: str) -> Path:
    """A ``--protocol`` value as a file: a path ending in ``.toml`` or with a directory, else
    the preset ``protocols/<name>.toml``."""
    if name.endswith(".toml") or os.sep in name or "/" in name:
        return Path(name)
    return protocol_module.PROTOCOLS_DIR / f"{name}.toml"


def main(args: argparse.Namespace) -> int:
    """Runs ``gth evaluate``; returns its exit status."""
    try:
        if args.record != (args.license is not None) or args.record != (
            args.checks_every is not None
        ):
            raise ConfigError("--license and --checks-every go with --record, and it needs both")
        if not isinstance(args.policy_kwargs, dict):
            raise ConfigError("--policy-kwargs must be a JSON object")
        run = evaluate(
            protocol_module.load(protocol_path(args.protocol)),
            args.policy,
            args.out,
            args.data_dir,
            policy_kwargs=args.policy_kwargs,
            record=Recording(license=args.license, checks_every=args.checks_every)
            if args.record
            else None,
            jobs=args.jobs,
            rom_dir=args.rom_dir,
            core_path=args.core_path,
            games_dir=args.games_dir,
            report=lambda line: print(line, file=sys.stderr, flush=True),
        )
    except EvaluationFailed as error:
        print(f"gth evaluate: {error}", file=sys.stderr)
        return 1
    except HarnessError as error:
        print(f"gth evaluate: {error}", file=sys.stderr)
        return 2
    print(run)
    return 0
