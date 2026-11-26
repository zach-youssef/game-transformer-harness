"""Training shards (docs/recording-format.md, "Training shards"): exporting recordings into
them, in parallel, and reading them back as numpy batches without PyTorch.

A shard holds one episode at the model's observation size and rate: each sample is an
observation and the action the player chose after seeing it, and `segment` marks where
dropped samples leave gaps. The replay and the
per-frame work (rendering, compression, decoding) are Rust (`_native.export_shard`,
`_native.decode_frames`); this module picks the files, runs one process per recording and
assembles batches. Shards hold the game's RAM variables for rewards and validation, but
`batches` leaves them out: game RAM is never a policy's input.
"""

from __future__ import annotations

import argparse
import json
import multiprocessing
import os
import sys
import tomllib
from collections.abc import Iterator, Sequence
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal

import numpy as np
import numpy.typing as npt

from gtharness import _native
from gtharness.env import core_library, find_content
from gtharness.errors import ConfigError, HarnessError

SHARD_VERSION = 1
"""The shard format version this module reads (`gth_record::SHARD_VERSION`)."""

SampleFilter = Literal["all", "playing"]
LABELS = ("direction", "buttons", "ram", "episode_frame", "segment", "confidence")
FRAME_LABELS = ("frame_direction", "frame_buttons", "frame_labelled")
_FILTERS = ("all", "playing")
_INTERPOLATIONS = ("area", "bilinear")
_U32 = 2**32
_EXIT_HOOK = "GTHARNESS_TEST_EXIT_IN_EXPORT"
"""Test hook: a worker exporting the recording named in this variable exits at once, as a
crashing core would (tests/test_shards.py)."""


@dataclass(frozen=True, kw_only=True)
class ShardOptions:
    """How recordings are rendered into shards. Every field but ``interpolation`` is a choice
    with no neutral default, so all are required.

    Args:
        obs_size: ``(height, width)`` of the observations.
        crop: ``(x, y, width, height)`` of the source frame, None for the whole frame, or
            "game" for each recording's ``game.toml`` crop.
        orientation: degrees clockwise (0, 90 or 270), or "game" for each recording's.
        frameskip: frames per sample (the model's rate), or "recorded" for the recording's own
            steps.
        filter: "all" keeps every labelled sample; "playing" keeps from the observation
            before the first input through game over.
        chunk_samples: samples per frame chunk: larger compresses better, smaller decodes less
            to reach one sample.
        interpolation: "area" or "bilinear".

    Raises:
        ConfigError: if a field is out of range, before anything is exported.
    """

    obs_size: tuple[int, int]
    crop: tuple[int, int, int, int] | Literal["game"] | None
    orientation: int | Literal["game"]
    frameskip: int | Literal["recorded"]
    filter: SampleFilter
    chunk_samples: int
    interpolation: str

    def __post_init__(self) -> None:
        def count(name: str, value: object, low: int = 1) -> None:
            if isinstance(value, bool) or not isinstance(value, int) or not low <= value < _U32:
                raise ConfigError(
                    f"{name} must be an integer from {low} to 2^32 - 1, not {value!r}"
                )

        if not isinstance(self.obs_size, tuple) or len(self.obs_size) != 2:
            raise ConfigError(f"obs_size must be (height, width), not {self.obs_size!r}")
        for name, value in zip(("obs height", "obs width"), self.obs_size, strict=True):
            count(name, value)
        if self.crop is not None and self.crop != "game":
            if not isinstance(self.crop, tuple) or len(self.crop) != 4:
                raise ConfigError(
                    f"crop must be (x, y, width, height), None or 'game', not {self.crop!r}"
                )
            for name, value, low in zip(
                ("crop x", "crop y", "crop width", "crop height"),
                self.crop,
                (0, 0, 1, 1),
                strict=True,
            ):
                count(name, value, low)
        if self.orientation not in ("game", 0, 90, 270) or isinstance(self.orientation, bool):
            raise ConfigError(f"orientation must be 0, 90, 270 or 'game', not {self.orientation!r}")
        if self.frameskip != "recorded":
            count("frameskip", self.frameskip)
        if self.filter not in _FILTERS:
            raise ConfigError(f"filter must be one of {_FILTERS}, not {self.filter!r}")
        count("chunk_samples", self.chunk_samples)
        if self.interpolation not in _INTERPOLATIONS:
            raise ConfigError(
                f"interpolation must be one of {_INTERPOLATIONS}, not {self.interpolation!r}"
            )


@dataclass(frozen=True)
class ExportResult:
    """One recording's export: the shard's counts (`_native.export_shard`), or the error."""

    recording: Path
    shard: Path
    summary: dict[str, Any] | None
    error: str | None


def export_shards(
    recordings: Sequence[str | os.PathLike[str]],
    out_dir: str | os.PathLike[str],
    data_dir: str | os.PathLike[str],
    options: ShardOptions,
    *,
    jobs: int,
    rom_dir: str | os.PathLike[str] | None = None,
    core_path: str | os.PathLike[str] | None = None,
) -> list[ExportResult]:
    """Exports each recording to ``out_dir/<recording name>.npz``, ``jobs`` recordings at a
    time, each in its own process (one core per process, one process per recording).

    Files are found as `RetroEnv.from_game` finds them: the content by SHA-1 under
    ``rom_dir``, BIOS files in ``data_dir/system`` and the core in ``data_dir/cores`` unless
    ``core_path`` is given. Raises `ConfigError` before exporting anything if a recording's
    files are missing or a shard already exists. A recording that fails to export (a desync,
    or a crash that kills its process) doesn't stop the others: its result carries the error
    and it leaves no shard.
    """
    if jobs < 1:
        raise ValueError(f"jobs must be at least 1, not {jobs}")
    out = Path(out_dir)
    tasks = [
        _task(Path(path), out, Path(data_dir), options, rom_dir, core_path) for path in recordings
    ]
    shards = [task["out_path"] for task in tasks]
    if len(set(shards)) != len(shards):
        raise ConfigError("two recordings have the same name, so their shards would collide")
    existing = [str(path) for path in shards if path.exists()]
    if existing:
        raise ConfigError(f"shards already exist: {existing}")
    out.mkdir(parents=True, exist_ok=True)
    if not tasks:
        return []
    # A fresh process per recording (spawn): a libretro core keeps global state and only one
    # may be loaded per process, and a core that crashes takes down only its own recording.
    with ThreadPoolExecutor(max_workers=min(jobs, len(tasks))) as threads:
        return list(threads.map(_run_isolated, tasks))


def _run_isolated(task: dict[str, Any]) -> ExportResult:
    """Exports one recording in a process of its own and turns any failure, including the
    process dying, into the result's error, removing the partial shard it may leave."""
    recording, shard = Path(task["recording_dir"]), Path(task["out_path"])
    try:
        with ProcessPoolExecutor(
            max_workers=1, mp_context=multiprocessing.get_context("spawn")
        ) as pool:
            return ExportResult(recording, shard, pool.submit(_export_one, task).result(), None)
    except Exception as error:  # Every failure belongs to this recording alone.
        Path(f"{shard}.partial").unlink(missing_ok=True)
        return ExportResult(recording, shard, None, f"{type(error).__name__}: {error}")


def _task(
    recording: Path,
    out: Path,
    data_dir: Path,
    options: ShardOptions,
    rom_dir: str | os.PathLike[str] | None,
    core_path: str | os.PathLike[str] | None,
) -> dict[str, Any]:
    """The arguments of `_native.export_shard` for ``recording``."""
    try:
        meta = tomllib.loads((recording / "meta.toml").read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ConfigError(f"{recording}: not a complete recording: {error}") from error
    config = _native.GameConfig.load(recording / "game.toml")
    what = f"{recording.name} ({meta.get('game_id')})"
    content = find_content(config, what, rom_dir) if "content" in meta else None
    height, width = options.obs_size
    return {
        "recording_dir": recording,
        "core_path": Path(core_path) if core_path else core_library(data_dir, config.core),
        "content_path": content,
        "system_dir": data_dir / "system",
        "out_path": out / f"{recording.name.removesuffix('.gthrec')}.npz",
        "obs_height": height,
        "obs_width": width,
        "crop": config.crop if options.crop == "game" else options.crop,
        "rotate": config.orientation if options.orientation == "game" else options.orientation,
        "interpolation": options.interpolation,
        "frameskip": None if options.frameskip == "recorded" else options.frameskip,
        "filter": options.filter,
        "chunk_samples": options.chunk_samples,
    }


def _export_one(task: dict[str, Any]) -> dict[str, Any]:
    if os.environ.get(_EXIT_HOOK) == Path(task["recording_dir"]).name:
        Path(f"{task['out_path']}.partial").write_bytes(b"cut short")
        os._exit(3)
    return _native.export_shard(**task)


class Shard:
    """One shard file, opened with `numpy.load`: labels load when first used, frames one
    chunk at a time. Close it (or use it as a context manager) to release the file."""

    def __init__(self, path: str | os.PathLike[str]):
        self.path = Path(path)
        self._npz = np.load(self.path, allow_pickle=False)
        try:
            self.meta: dict[str, Any] = json.loads(self._npz["meta.json"])
            if self.meta.get("shard_version") != SHARD_VERSION:
                raise HarnessError(
                    f"{self.path}: shard version {self.meta.get('shard_version')}, but this"
                    f" harness reads {SHARD_VERSION}"
                )
        except BaseException:
            self._npz.close()
            raise
        self._labels: dict[str, np.ndarray] = {}

    def __len__(self) -> int:
        return int(self.meta["samples"])

    def __enter__(self) -> Shard:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    def close(self) -> None:
        self._npz.close()

    @property
    def obs_size(self) -> tuple[int, int]:
        height, width = self.meta["obs_size"]
        return int(height), int(width)

    @property
    def frame_chunks(self) -> int:
        return int(self.meta["frame_chunks"])

    def chunk_range(self, chunk: int) -> range:
        """The samples frame chunk ``chunk`` holds."""
        if not 0 <= chunk < self.frame_chunks:
            raise IndexError(f"{self.path}: no frame chunk {chunk} of {self.frame_chunks}")
        step = int(self.meta["chunk_samples"])
        return range(chunk * step, min((chunk + 1) * step, len(self)))

    def frames(self, chunk: int) -> npt.NDArray[np.uint8]:
        """Frame chunk ``chunk`` as ``uint8[n, height, width, 3]``, decoded."""
        samples = self.chunk_range(chunk)
        data = self._npz[f"frames_{chunk:05}.zst"]
        return _native.decode_frames(data, len(samples), *self.obs_size, self.path)

    def label(self, name: str) -> np.ndarray:
        """Per-sample array ``name``: direction, buttons, ram (``int64[n, len(ram_names)]``,
        for rewards and validation, never a policy's input), episode_frame, segment or
        confidence; or per-frame array frame_direction, frame_buttons or frame_labelled (one
        entry per replayed frame, index f - 1 for frame f)."""
        if name in LABELS:
            length, what = len(self), "samples"
        elif name in FRAME_LABELS:
            length, what = int(self.meta["frames"]), "frames"
        else:
            raise KeyError(f"no array {name!r}; there are {LABELS + FRAME_LABELS}")
        if name not in self._labels:
            array = self._npz[name]
            if len(array) != length:
                raise HarnessError(f"{self.path}: {name} has {len(array)} {what}, not {length}")
            self._labels[name] = array
        return self._labels[name]

    @property
    def direction(self) -> npt.NDArray[np.uint8]:
        return self.label("direction")

    @property
    def buttons(self) -> npt.NDArray[np.uint8]:
        return self.label("buttons")

    @property
    def ram(self) -> npt.NDArray[np.int64]:
        return self.label("ram")

    @property
    def episode_frame(self) -> npt.NDArray[np.uint64]:
        return self.label("episode_frame")

    @property
    def segment(self) -> npt.NDArray[np.uint32]:
        """Consecutive samples share a segment; it goes up at each gap dropped samples leave."""
        return self.label("segment")

    @property
    def confidence(self) -> npt.NDArray[np.float32]:
        return self.label("confidence")

    @property
    def frame_direction(self) -> npt.NDArray[np.uint8]:
        return self.label("frame_direction")

    @property
    def frame_buttons(self) -> npt.NDArray[np.uint8]:
        return self.label("frame_buttons")

    @property
    def frame_labelled(self) -> npt.NDArray[np.bool_]:
        return self.label("frame_labelled")


def batches(
    paths: Sequence[str | os.PathLike[str]],
    batch_size: int,
    *,
    rng: np.random.Generator | None,
    buffer_chunks: int,
) -> Iterator[dict[str, np.ndarray]]:
    """Every sample of the shards at ``paths`` once, as batches of ``batch_size`` (the last
    may be smaller): ``frame`` (``uint8[b, height, width, 3]``), its label ``direction`` and
    ``buttons``, ``episode_frame``, ``segment``, ``confidence``, and ``shard`` and ``sample``,
    which locate each sample (an index into ``paths`` and into that shard). RAM isn't in a
    batch; a reward reads it with `Shard.ram` at those indices.

    With ``rng``, frame chunks are visited in random order and samples are shuffled within
    ``buffer_chunks`` decoded chunks at a time (more mixes better and holds more frames in
    memory); without, samples come in order. Every shard must have the same observation size.
    """
    if batch_size < 1 or buffer_chunks < 1:
        raise ValueError("batch_size and buffer_chunks must be at least 1")
    shards = [Shard(path) for path in paths]
    try:
        sizes = {shard.obs_size for shard in shards}
        if len(sizes) > 1:
            raise HarnessError(f"the shards have different observation sizes: {sorted(sizes)}")
        order = [(s, c) for s, shard in enumerate(shards) for c in range(shard.frame_chunks)]
        if rng is not None:
            rng.shuffle(order)
        pending: dict[str, np.ndarray] | None = None
        for start in range(0, len(order), buffer_chunks):
            parts = [_chunk(shards, s, c) for s, c in order[start : start + buffer_chunks]]
            if pending is not None:
                parts.insert(0, pending)
            buffer = {key: np.concatenate([part[key] for part in parts]) for key in parts[0]}
            if rng is not None:
                permutation = rng.permutation(len(buffer["frame"]))
                buffer = {key: value[permutation] for key, value in buffer.items()}
            full = len(buffer["frame"]) // batch_size * batch_size
            for at in range(0, full, batch_size):
                yield {key: value[at : at + batch_size] for key, value in buffer.items()}
            pending = {key: value[full:] for key, value in buffer.items()}
        if pending is not None and len(pending["frame"]):
            yield pending
    finally:
        for shard in shards:
            shard.close()


def _chunk(shards: list[Shard], s: int, chunk: int) -> dict[str, np.ndarray]:
    shard = shards[s]
    samples = shard.chunk_range(chunk)
    part = {"frame": shard.frames(chunk)}
    for name in ("direction", "buttons", "episode_frame", "segment", "confidence"):
        part[name] = shard.label(name)[samples.start : samples.stop]
    part["shard"] = np.full(len(samples), s, dtype=np.int64)
    part["sample"] = np.arange(samples.start, samples.stop, dtype=np.int64)
    return part


def add_parser(commands: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    """Adds ``gth export-shards`` to the ``gth`` command line."""
    parser = commands.add_parser(
        "export-shards",
        help="render recordings into training shards",
        description="Replays each recording and writes OUT/<recording>.npz (docs/"
        "recording-format.md, 'Training shards'), one process per recording. Exit status: 0 "
        "when every recording exported, 1 when one failed (it leaves no shard), 2 on a usage "
        "or config error.",
    )
    parser.add_argument("data_dir", type=Path, help="data directory, as for RetroEnv.from_game")
    parser.add_argument("recordings", type=Path, nargs="+", help="recording directories")
    parser.add_argument("--out", type=Path, required=True, help="directory for the shards")
    parser.add_argument("--obs-size", type=int, nargs=2, metavar=("HEIGHT", "WIDTH"), required=True)
    parser.add_argument(
        "--crop",
        nargs="+",
        required=True,
        metavar="CROP",
        help="'game' (each recording's game.toml), 'none', or X Y WIDTH HEIGHT",
    )
    parser.add_argument(
        "--orientation", choices=["game", "0", "90", "270"], required=True, help="degrees clockwise"
    )
    parser.add_argument(
        "--frameskip",
        required=True,
        help="frames per sample, or 'recorded' for the recording's steps",
    )
    parser.add_argument("--filter", choices=["all", "playing"], required=True)
    parser.add_argument("--chunk-samples", type=int, required=True, help="samples per frame chunk")
    parser.add_argument("--interpolation", choices=["area", "bilinear"], default="area")
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 1, help="default: CPU count")
    parser.add_argument(
        "--rom-dir", type=Path, help="default: GTH_ROM_DIR or rom_dir in the user config"
    )
    parser.add_argument("--core-path", type=Path, help="default: data_dir/cores/")
    parser.add_argument("--json", action="store_true", help="print the results as JSON")


def _options(args: argparse.Namespace) -> ShardOptions:
    crop: tuple[int, int, int, int] | Literal["game"] | None
    if args.crop == ["game"]:
        crop = "game"
    elif args.crop == ["none"]:
        crop = None
    elif len(args.crop) == 4 and all(part.isdigit() for part in args.crop):
        x, y, width, height = (int(part) for part in args.crop)
        crop = (x, y, width, height)
    else:
        raise ConfigError(f"--crop must be 'game', 'none' or X Y WIDTH HEIGHT, not {args.crop}")
    if args.frameskip != "recorded" and not args.frameskip.isdigit():
        raise ConfigError(f"--frameskip must be a number or 'recorded', not {args.frameskip!r}")
    return ShardOptions(
        obs_size=(args.obs_size[0], args.obs_size[1]),
        crop=crop,
        orientation="game" if args.orientation == "game" else int(args.orientation),
        frameskip="recorded" if args.frameskip == "recorded" else int(args.frameskip),
        filter=args.filter,
        chunk_samples=args.chunk_samples,
        interpolation=args.interpolation,
    )


def main(args: argparse.Namespace) -> int:
    """Runs ``gth export-shards``; returns its exit status."""
    try:
        results = export_shards(
            args.recordings,
            args.out,
            args.data_dir,
            _options(args),
            jobs=args.jobs,
            rom_dir=args.rom_dir,
            core_path=args.core_path,
        )
    except (HarnessError, ValueError, OverflowError) as error:
        print(f"gth export-shards: {error}", file=sys.stderr)
        return 2
    if args.json:
        rows = [
            {"recording": str(r.recording), "shard": str(r.shard), "error": r.error}
            | ({k: str(v) if k == "path" else v for k, v in r.summary.items()} if r.summary else {})
            for r in results
        ]
        print(json.dumps(rows, indent=2))
    else:
        for r in results:
            if r.summary is not None:
                print(
                    f"{r.shard}: {r.summary['samples']} samples, {r.summary['bytes']} bytes"
                    f" ({r.summary['unlabelled']} unlabelled, {r.summary['filtered']} filtered)"
                )
            else:
                print(f"{r.recording}: failed: {r.error}")
    return 0 if all(r.error is None for r in results) else 1
