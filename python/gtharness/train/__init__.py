"""Training data over shards (docs/training.md): numpy only, so it runs wherever the harness
does; the PyTorch side builds on it."""

from gtharness.train.windows import ActionFormat, Windows, returns_to_go, sample_rewards

__all__ = ["ActionFormat", "Windows", "returns_to_go", "sample_rewards"]
