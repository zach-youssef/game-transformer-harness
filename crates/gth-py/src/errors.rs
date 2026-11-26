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
    ConfigError,
    HarnessError,
    "A game config, the user config or a file it names is invalid or missing."
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
    Config(gth_config::Error),
    Core(gth_core::Error),
    Frame(gth_frame::Error),
    Record(gth_record::Error),
    Python(PyErr),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<gth_core::Error> for Error {
    fn from(error: gth_core::Error) -> Self {
        Self::Core(error)
    }
}

impl From<gth_config::Error> for Error {
    fn from(error: gth_config::Error) -> Self {
        Self::Config(error)
    }
}

impl From<gth_frame::Error> for Error {
    fn from(error: gth_frame::Error) -> Self {
        Self::Frame(error)
    }
}

impl From<gth_record::Error> for Error {
    fn from(error: gth_record::Error) -> Self {
        Self::Record(error)
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
            Error::Config(error) => ConfigError::new_err(error.to_string()),
            // A bad crop or output size from the caller, or a frame the core shouldn't send.
            Error::Frame(error) => HarnessError::new_err(error.to_string()),
            Error::Core(error) => core_error(error),
            Error::Record(error) => {
                let message = error.to_string();
                // No wildcard arm: a new gth_record::Error variant must pick its exception here.
                match error {
                    gth_record::Error::Desync { .. } => DesyncError::new_err(message),
                    // The recording doesn't fit the core or content it is replayed with.
                    gth_record::Error::Mismatch { .. } => ConfigError::new_err(message),
                    gth_record::Error::Io { .. }
                    | gth_record::Error::Format { .. }
                    | gth_record::Error::Failed { .. } => HarnessError::new_err(message),
                    gth_record::Error::Core(error) => core_error(error),
                    gth_record::Error::Config(error) => ConfigError::new_err(error.to_string()),
                }
            }
        }
    }
}

fn core_error(error: gth_core::Error) -> PyErr {
    let message = error.to_string();
    // No wildcard arm: a new gth_core::Error variant must pick its exception here.
    match error {
        gth_core::Error::CoreLoad { .. } => CoreLoadError::new_err(message),
        // A file the caller (or a game config) names: a config problem.
        gth_core::Error::Content { .. } => ConfigError::new_err(message),
        gth_core::Error::Savestate { .. } => HarnessError::new_err(message),
        gth_core::Error::Ram { .. } => HarnessError::new_err(message),
    }
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    module.add("HarnessError", py.get_type::<HarnessError>())?;
    module.add("CoreLoadError", py.get_type::<CoreLoadError>())?;
    module.add("ConfigError", py.get_type::<ConfigError>())?;
    module.add("DesyncError", py.get_type::<DesyncError>())?;
    Ok(())
}
