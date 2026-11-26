# game-transformer-harness

Headless [libretro](https://www.libretro.com/) emulator cores behind a Python
[gymnasium](https://gymnasium.farama.org/) environment, with all per-frame work (emulation
callbacks, pixel conversion, resize, frame skip, recording) in Rust. It is built to run game
policies such as NitroGen and to record human play as exact training data.

Status: scaffolding. The crates are empty; see [docs/architecture.md](docs/architecture.md) for
what goes where.

## Build

Needs [rustup](https://rustup.rs/) and [uv](https://docs.astral.sh/uv/):

```sh
uv sync
uv run maturin develop --uv
uv run pytest
```

More in [docs/development.md](docs/development.md).

## ROMs

No ROMs, BIOS files or cores are included or downloaded: you supply your own dumps
([docs/roms.md](docs/roms.md)).

## License

MIT, see [LICENSE](LICENSE). Emulator cores loaded at runtime have their own licenses.
