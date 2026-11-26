# Development

## Tools

- Rust: install [rustup](https://rustup.rs/). `rust-toolchain.toml` pins the compiler, and
  rustup installs it on first use.
- Python: install [uv](https://docs.astral.sh/uv/). It installs Python 3.12 (`.python-version`)
  and the dev tools (maturin, pytest, ruff) pinned in `uv.lock`.

There are no prebuilt wheels: every machine builds the extension module itself.

## Commands

```sh
uv sync                        # create .venv with Python 3.12 and the dev tools
uv run maturin develop --uv    # build gtharness._native (debug) and install it into .venv
uv run pytest                  # Python tests
uv run ruff check              # lint
uv run ruff format             # format (CI runs `ruff format --check`)

cargo fmt --all                # format Rust (CI runs `cargo fmt --all --check`)
export PYO3_BUILD_EXTENSION_MODULE=1   # PowerShell: $env:PYO3_BUILD_EXTENSION_MODULE = "1"
uv run cargo clippy --workspace --all-targets -- -D warnings
uv run cargo test --workspace
```

- Run clippy and tests through `uv run`, so PyO3's build script uses the venv's Python 3.12.
  A plain `cargo` takes whatever `python3` is on `PATH`, which may be older than 3.12 (the
  Mac's is 3.9) or missing, and the build fails.
- `PYO3_BUILD_EXTENSION_MODULE=1` builds `gth-py` as an extension module, as `maturin` does,
  so cargo never links libpython (which Linux runners don't have).

- uv doesn't build the package (`package = false` in `pyproject.toml`); `maturin develop` does.
  Rerun it after changing Rust code. Add `--release` when measuring speed.
- `uv sync` removes packages it didn't install, including the one `maturin develop` installed,
  so rerun `maturin develop` after it. `uv run` doesn't remove anything.
- `gth-py` has no Rust tests (`test = false`): it is glue, and pytest tests it through Python.

## Build directory

Rust builds are large. On a machine with several checkouts or worktrees of this repo, share one
build directory between them by setting `CARGO_TARGET_DIR` (cargo's lock serializes builds that
share it), and run `cargo clean` when a task is done if disk is short. `maturin develop` uses
the same directory.
