//! Test hooks (underscore names): they let pytest drive a convention before a real binding
//! uses it. Nothing outside `tests/` calls them.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::errors;

/// Raises the exception that `kind` ("core_load" or "desync") of `gth_core::Error` maps to.
#[pyfunction]
fn _raise_core_error(kind: &str) -> errors::Result<()> {
    let error = match kind {
        "core_load" => gth_core::Error::CoreLoad {
            path: "missing.so".into(),
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

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_raise_core_error, module)?)?;
    Ok(())
}
