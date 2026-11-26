//! Test hooks (underscore names): they let pytest drive a convention before a real binding
//! uses it. Nothing outside `tests/` calls them.

use std::time::Duration;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::errors;

/// Raises the exception that `kind` ("core_load", "savestate" or "desync") of `gth_core::Error` maps to.
#[pyfunction]
fn _raise_core_error(kind: &str) -> errors::Result<()> {
    let error = match kind {
        "core_load" => gth_core::Error::CoreLoad {
            path: "missing.so".into(),
            reason: "test".into(),
        },
        "savestate" => gth_core::Error::Savestate {
            core: "x".into(),
            reason: "test".into(),
        },
        "desync" => gth_core::Error::Desync {
            frame: 7,
            detail: "test".into(),
        },
        _ => {
            let message = format!("unknown error kind {kind:?}");
            return Err(PyValueError::new_err(message).into());
        }
    };
    Err(error.into())
}

/// Blocks for `seconds` with the GIL released, standing in for a long native call.
#[pyfunction]
fn _sleep_detached(py: Python<'_>, seconds: f64) -> errors::Result<()> {
    let duration =
        Duration::try_from_secs_f64(seconds).map_err(|e| PyValueError::new_err(e.to_string()))?;
    py.detach(|| std::thread::sleep(duration));
    Ok(())
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_raise_core_error, module)?)?;
    module.add_function(wrap_pyfunction!(_sleep_detached, module)?)?;
    Ok(())
}
