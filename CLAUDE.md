# game-transformer-harness

Headless libretro cores behind a Python gymnasium environment, with all per-frame work in Rust
(PyO3 extension `gtharness._native`, built with maturin). Read the doc for the area you touch:

- [docs/architecture.md](docs/architecture.md): crate map, data flow, one core per process,
  where hot code lives. Read before adding code anywhere.
- [docs/development.md](docs/development.md): uv, maturin and cargo commands, CI, the shared
  `CARGO_TARGET_DIR`.
- [docs/native.md](docs/native.md): the `_native` binding conventions and type stubs. Read
  before touching `gth-py` or `_native.pyi`.
- [docs/libretro.md](docs/libretro.md): the libretro FFI subset in `gth-libretro-sys`, how to
  extend it, and what the test core does. Read before touching either crate, the host's
  callbacks or tests that load a core.
- [docs/games.md](docs/games.md): the `game.toml` and `cores.lock` schemas (`gth-config`).
  Read before writing or changing a game config.
- [docs/actions.md](docs/actions.md): the action space, slot convention and NitroGen mapping.
- [docs/roms.md](docs/roms.md): before touching ROMs, BIOS or cores, or tests that use them.

Rules that apply everywhere:

- Never download, commit or upload ROMs, BIOS files or core binaries.
- Per-frame work is Rust; Python only orchestrates. `gth-py` is glue with no logic.
- Every `unsafe` block has a `// SAFETY:` comment.
- Run what CI runs before committing (docs/development.md).
