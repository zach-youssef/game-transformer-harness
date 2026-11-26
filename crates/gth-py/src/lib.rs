//! PyO3 glue for `gtharness._native`: type and error conversion only; logic lives in other crates.

use pyo3::prelude::*;

/// The `gtharness._native` extension module. Later stack PRs add its bindings.
#[pymodule]
fn _native(_module: &Bound<'_, PyModule>) -> PyResult<()> {
    Ok(())
}
