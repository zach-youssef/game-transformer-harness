"""JAT (Jack of All Trades, Gallouédec et al. 2024) as a `Policy`: the released checkpoint
played on Atari-style observations (docs/evaluation.md, "Adapters").

The model is the hub's ``jat-project/jat``, loaded from a local directory with its own code
(``trust_remote_code``) by transformers; torch and transformers come from the ``jat`` extra
(``uv sync --extra jat``) and are imported only when the policy is made, so this module
imports without them. The weights are never part of the harness: the user or an experiment
fetches them, and the policy reports their SHA-256.

JAT saw Atari through upstream's ``make_atari`` (jat repo ``jat/eval/rl/core.py``): frame
skip 4 with the max of the last two frames, 84x84 frames resized with OpenCV's area filter
and then made gray, four of them stacked channels-last, and each game's minimal action set
(ALE v5's default). The policy asks the protocol for the same observations and turns the
runner's ``uint8[4, 84, 84, 1]`` stack into that ``[84, 84, 4]`` image.
"""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
from typing import Any, Protocol

import numpy as np
import numpy.typing as npt

from gtharness.policies import GameInfo, ObsRequirements

__all__ = ["CODE_SHA256", "FRAME_STACK", "JatPolicy", "sha256_file"]

FRAME_STACK = 4
"""Frames in one JAT image observation: its image encoder takes four channels."""

CODE_SHA256 = {
    "config.json": "d07b0e1b6e226be784bbb7ead6c1abba44087087bfeba876f31b7c9698c29959",
    "configuration_jat.py": "98f6fd4cb2eef67e55e73c04a0608c890e720d873ffd8d3165801a97245fe3c8",
    "generation_config.json": "7b114e7167d77ff3546ab755cd62833307b6699d6cbc380a26643b4bc8ba1b31",
    "merges.txt": "1ce1664773c50f3e0cc8842619a93edc4624525b728b188a9e0be33b7726adc5",
    "modeling_jat.py": "0e488ceac5fd4b64bc8d56ddb6974ec0592a20e5b8498352094a21047c7aa58d",
    "preprocessor_config.json": "af442611108a62ae3706999003d15e92835a45735631bd75944992822ce1712c",
    "processing_jat.py": "da6e0a0d5d4fec1865f5475c35a26774bd2783822ee3adc1d8af8ec3716ab158",
    "special_tokens_map.json": "aff38493227d813e29fcf8406e8e90062f1f031aa47d589325e9c31d89ac7cc3",
    "tokenizer.json": "50481e91e5a59f93b1826b92d3e220699725e82a538dc821e61fd372c39c089c",
    "tokenizer_config.json": "8e6e3fc5cda25ccf9b216ace861652fce3a0d73eba8fbed475a43d758f35f8ac",
    "vocab.json": "3ba3c3109ff33976c4bd966589c11ee14fcaa1f4c9e5e154c2ed7f99d80709e7",
}
"""Every file of hub revision ``911dd7447b1c`` but the weights: the code the policy drives
(``get_next_action``, the KV-cache trimming), which the research experiment
``2026-11-24-jat-mac-smoke`` checked, and the configs and tokenizer files that loading reads
(their ``auto_map`` entries name code to run). A directory where any differs is refused:
loading runs that code, and it isn't the model this adapter was written against."""

ACTION_SETS = ("atari_full", "atari_minimal")
DEVICES = ("cpu", "mps", "cuda")


def sha256_file(path: str | os.PathLike[str]) -> str:
    """The file's SHA-256, lowercase hex."""
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        while block := file.read(1 << 20):
            digest.update(block)
    return digest.hexdigest()


class Backend(Protocol):
    """The model behind `JatPolicy`: the real one (torch) or a test's stand-in."""

    max_position_embeddings: int
    """The checkpoint's sequence length, in positions; a JAT timestep takes two."""

    def reset(self, seed: int | None) -> None:
        """Clears the KV cache; an int seeds the sampling."""
        ...

    def act(
        self,
        image: npt.NDArray[np.uint8],
        n_actions: int,
        context_positions: int,
        deterministic: bool,
    ) -> int:
        """The action for ``image`` (``uint8[84, 84, 4]``), in ``0..n_actions``, keeping
        the last ``context_positions`` cached positions as context: the most likely one,
        or a sample."""
        ...


class JatPolicy:
    """JAT on 84x84 gray, max-pooled, 4-frame stacks at frame skip 4, in an Atari action set.

    Every argument is a choice that changes the scores, so none has a default:

    - ``checkpoint``: the directory of the hub snapshot (``model.safetensors``, the configs
      and the code, as ``huggingface_hub.snapshot_download`` leaves it); its code must be
      `CODE_SHA256`'s.
    - ``action_set``: ``"atari_minimal"`` (a game's minimal set, as JAT was trained and
      evaluated on Atari) or ``"atari_full"`` (ALE's 18, for games without a minimal set).
      The action head is cut to the game's ``n_actions`` as upstream cuts it to the env's.
    - ``context_timesteps``: how many past timesteps the KV cache keeps (two positions
      each); upstream's Atari evaluation keeps 16 (``context_window = 32``), the checkpoint
      at most ``max_position_embeddings // 2`` (256).
    - ``deterministic``: the most likely action instead of a sample; upstream samples.
    - ``device`` (``"cpu"``, ``"mps"``, ``"cuda"``) and ``threads`` (torch's CPU threads:
      the runner's ``--jobs`` processes each hold a model, so pick ``threads`` with it).

    One checkpoint serves every policy seed, so the protocol's policy seeds are sampling
    seeds of one model (docs/metrics.md, "Runs"). Sampling uses torch's global RNG, which
    `reset` seeds: the runner plays one policy per process.
    """

    id = "jat-v1"
    """Bumped whenever the actions chosen for the same observations, settings and seed
    change."""
    code_sha256: dict[str, str] = CODE_SHA256
    """The code and configs the checkpoint directory must hold (tests' stand-ins differ)."""

    def __init__(
        self,
        *,
        policy_seed: int,
        checkpoint: str | os.PathLike[str],
        action_set: str,
        context_timesteps: int,
        deterministic: bool,
        device: str,
        threads: int,
    ) -> None:
        del policy_seed  # one checkpoint for every seed; `reset` seeds the sampling
        if action_set not in ACTION_SETS:
            raise ValueError(f"action_set must be one of {ACTION_SETS}, not {action_set!r}")
        if device not in DEVICES:
            raise ValueError(f"device must be one of {DEVICES}, not {device!r}")
        if not isinstance(threads, int) or isinstance(threads, bool) or threads < 1:
            raise ValueError(f"threads must be an integer >= 1, not {threads!r}")
        if not isinstance(deterministic, bool):
            raise ValueError(f"deterministic must be a bool, not {deterministic!r}")
        directory = Path(checkpoint)
        for name, wanted in self.code_sha256.items():
            path = directory / name
            if not path.is_file():
                raise FileNotFoundError(f"{directory}: no {name}: not a JAT hub snapshot")
            if sha256_file(path) != wanted:
                raise ValueError(
                    f"{path}: SHA-256 differs from revision 911dd7447b1c's, which this"
                    " adapter drives (CODE_SHA256)"
                )
        weights = directory / "model.safetensors"
        if not weights.is_file():
            raise FileNotFoundError(f"{directory}: no model.safetensors")
        self.checkpoint_sha256 = sha256_file(weights)
        self.obs_requirements = ObsRequirements(
            size=(84, 84),
            grayscale=True,
            maxpool=True,
            frame_stack=FRAME_STACK,
            frameskip=4,
            action_set=action_set,
        )
        self._backend = self._load(directory, device=device, threads=threads)
        limit = self._backend.max_position_embeddings // 2
        if (
            not isinstance(context_timesteps, int)
            or isinstance(context_timesteps, bool)
            or not 1 <= context_timesteps <= limit
        ):
            raise ValueError(
                f"context_timesteps must be an integer in 1..{limit} (the checkpoint's"
                f" max_position_embeddings // 2), not {context_timesteps!r}"
            )
        self._context_positions = 2 * context_timesteps
        self._deterministic = deterministic
        self._n_actions = 0

    def _load(self, directory: Path, *, device: str, threads: int) -> Backend:
        """The model; tests override this with a stand-in that needs no torch."""
        return _TorchBackend(directory, device=device, threads=threads)

    def reset(self, seed: int | None, game: GameInfo) -> None:
        self._backend.reset(seed)
        self._n_actions = game.n_actions

    def act(self, obs: npt.NDArray[np.uint8]) -> int:
        if obs.shape != (FRAME_STACK, 84, 84, 1) or obs.dtype != np.uint8:
            raise ValueError(f"JAT needs uint8[4, 84, 84, 1] stacks, not {obs.dtype}{obs.shape}")
        # Oldest first in both: the runner's stack and gymnasium's FrameStack.
        image = np.ascontiguousarray(obs[..., 0].transpose(1, 2, 0))
        return self._backend.act(
            image, self._n_actions, self._context_positions, self._deterministic
        )


class _TorchBackend:
    """The checkpoint through its own ``get_next_action``, as upstream's evaluation calls it
    (jat repo ``scripts/eval_jat.py``)."""

    def __init__(self, directory: Path, *, device: str, threads: int) -> None:
        try:
            import torch
            from gymnasium import spaces
            from transformers import AutoModelForCausalLM, AutoProcessor
        except ImportError as error:
            raise ImportError(
                f"the JAT policy needs the jat extra (uv sync --extra jat): {error}"
            ) from error
        torch.set_num_threads(threads)
        model, info = AutoModelForCausalLM.from_pretrained(
            directory, trust_remote_code=True, local_files_only=True, output_loading_info=True
        )
        unloaded = {key: info[key] for key in ("missing_keys", "unexpected_keys") if info[key]}
        if unloaded:
            raise ValueError(f"{directory}: the weights don't fit the model: {unloaded}")
        self._model: Any = model.to(device).eval()
        self._processor = AutoProcessor.from_pretrained(
            directory, trust_remote_code=True, local_files_only=True
        )
        self._torch = torch
        self._spaces = spaces
        self.max_position_embeddings = int(model.config.max_position_embeddings)

    def reset(self, seed: int | None) -> None:
        self._model.reset_rl()
        if seed is not None:
            self._torch.manual_seed(seed)

    def act(
        self,
        image: npt.NDArray[np.uint8],
        n_actions: int,
        context_positions: int,
        deterministic: bool,
    ) -> int:
        # The model ignores rewards next to image observations (its embed_rl), so none is
        # passed; the runner gives policies none either.
        return int(
            self._model.get_next_action(
                self._processor,
                image_observation=image,
                action_space=self._spaces.Discrete(n_actions),
                reward=None,
                deterministic=deterministic,
                context_window=context_positions,
            )
        )
