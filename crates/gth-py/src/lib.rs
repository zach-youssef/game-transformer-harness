//! PyO3 glue for `gtharness._native`: type and error conversion only; logic lives in other crates.
//! The binding conventions are in docs/native.md; `python/gtharness/_native.pyi` mirrors this
//! module.

mod config;
mod core;
mod cores;
mod errors;
mod frame;
mod ramsearch;
mod record;
mod shard;
mod testing;

use std::collections::BTreeMap;

use pyo3::prelude::*;

/// Build metadata recorded with datasets: package version, git commit and whether the Rust
/// sources differed from it, target triple, cargo profile and rustc version. Every `gth-*`
/// crate shares the workspace version. Sorted by key, so serialized metadata is stable.
#[pyfunction]
pub(crate) fn build_info() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("version", env!("CARGO_PKG_VERSION")),
        ("git_sha", env!("GTH_GIT_SHA")),
        ("git_dirty", env!("GTH_GIT_DIRTY")),
        ("target", env!("GTH_TARGET")),
        ("profile", env!("GTH_PROFILE")),
        ("rustc", env!("GTH_RUSTC")),
    ])
}

/// The `gtharness._native` extension module.
#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add_function(wrap_pyfunction!(build_info, module)?)?;
    config::register(module)?;
    core::register(module)?;
    cores::register(module)?;
    errors::register(module)?;
    frame::register(module)?;
    ramsearch::register(module)?;
    record::register(module)?;
    shard::register(module)?;
    testing::register(module)?;
    Ok(())
}
