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
  Rerun it after changing Rust code. It builds debug, so pytest timings are debug numbers:
  add `--release` when measuring speed.
- `uv sync` removes packages it didn't install, including the one `maturin develop` installed,
  so rerun `maturin develop` after it. `uv run` doesn't remove anything.
- CI passes `--locked`, so after adding or changing a Rust dependency, update `Cargo.lock`
  (any plain `cargo build`, `cargo metadata` or `cargo fetch`) and commit it with the change.
- New dependencies and version bumps take a release at least two weeks old, so a broken or
  malicious release has time to be noticed and yanked. `zstd`, which writes every recording,
  is pinned with `=` in `gth-record`'s `Cargo.toml` so a routine `cargo update` can't move
  it; its `zstd-safe` and `zstd-sys` stay pinned by `Cargo.lock` (`--locked`).
- `gth-py` has no Rust tests (`test = false`): it is glue, and pytest tests it through Python.
- CI (`.github/workflows/ci.yml`) runs the commands above on Linux, macOS (arm64) and
  Windows, on every pushed branch (a PR shows its branch's push run). CI has no ROMs and no
  real cores. `astral-sh/setup-uv` publishes no major-version tag, so the workflow pins an
  exact release and bumping it is manual. It also pins uv itself to the version that wrote
  `uv.lock`; bump both together.

## Build directory

Rust builds are large, and `maturin develop` uses the same directory as cargo
(`CARGO_TARGET_DIR`, default `target/`). Give each checkout or worktree its own: cargo decides
what to rebuild for path crates by file times, so a worktree sharing a target directory with
another can get a stale build of a crate the other last built. If disk forces sharing, touch a
source file of the changed crate before trusting a rebuild. Run `cargo clean` when a task is
done if disk is short.

## Commits and stacks

Every commit passes the CI checks on its own (docs/testing.md, "Per-commit checks"). In a
stack of commits:

- A crate commit that adds a `gth_core` or `gth_record` error variant before the `gth-py`
  commit that maps it maps it to `HarnessError` (docs/native.md, "Errors"), since the mapping
  has no wildcard arm and the crate's commit must build; the `gth-py` commit then gives it its
  own exception if it needs one.
- Fixups to a list every commit touches, such as the rustfmt-wrapped `pub use` list in
  `crates/gth-config/src/lib.rs`, conflict at every later commit during
  `git rebase --autosquash`. Fold such a fixup into the last commit that touches the list, or
  resolve the conflict once and keep the result.

## Clippy

CI runs clippy with `-D warnings` on the pinned toolchain (`rust-toolchain.toml`), whose
lints may be newer than you expect. Rust 1.100's clippy flags `slice.chunks_exact(N)` with a
constant `N` (use `as_chunks`) and `x % n == 0` (use `x.is_multiple_of(n)`).
