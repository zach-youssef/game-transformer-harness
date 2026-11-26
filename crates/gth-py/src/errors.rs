//! The harness's Python exceptions, and the conversion of crate errors into them.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

// Named `gtharness.errors.*`, where Python code imports them from (and pickle finds them).
create_exception!(
    gtharness.errors,
    HarnessError,
    PyException,
    "Base class of every error the harness raises."
);
create_exception!(
    gtharness.errors,
    CoreLoadError,
    HarnessError,
    "A libretro core couldn't be loaded or initialized."
);
create_exception!(
    gtharness.errors,
    DesyncError,
    HarnessError,
    "A replay produced a different state than its recording."
);

/// The error type of every `#[pyfunction]` (through [`Result`]). `?` converts crate errors and
/// `PyErr` into it, and PyO3 raises it as the matching Python exception. It exists because the
/// orphan rule forbids `impl From<gth_core::Error> for PyErr` in this crate.
pub enum Error {
    Core(gth_core::Error),
    Python(PyErr),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<gth_core::Error> for Error {
    fn from(error: gth_core::Error) -> Self {
        Self::Core(error)
    }
}

impl From<PyErr> for Error {
    fn from(error: PyErr) -> Self {
        Self::Python(error)
    }
}

impl From<Error> for PyErr {
    fn from(error: Error) -> Self {
        match error {
            Error::Python(error) => error,
            Error::Core(error) => {
                let message = error.to_string();
                // No wildcard arm: a new gth_core::Error variant must pick its exception here.
                match error {
                    gth_core::Error::CoreLoad { .. } => CoreLoadError::new_err(message),
                    gth_core::Error::Desync { .. } => DesyncError::new_err(message),
                }
            }
        }
    }
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    module.add("HarnessError", py.get_type::<HarnessError>())?;
    module.add("CoreLoadError", py.get_type::<CoreLoadError>())?;
    module.add("DesyncError", py.get_type::<DesyncError>())?;
    Ok(())
}
