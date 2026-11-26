//! `_native.CoresLock`: a parsed `cores.lock`, and the checks `gth cores` runs on a core build
//! (docs/cores.md).

use std::collections::BTreeMap;
use std::path::PathBuf;

use pyo3::prelude::*;

use crate::errors;

#[pyclass(module = "gtharness._native", frozen)]
pub struct CoresLock {
    pub(crate) lock: gth_config::CoresLock,
}

#[pymethods]
impl CoresLock {
    /// Reads and validates a `cores.lock`.
    #[staticmethod]
    fn load(py: Python<'_>, path: PathBuf) -> errors::Result<Self> {
        let lock = py.detach(|| gth_config::CoresLock::load(&path))?;
        Ok(CoresLock { lock })
    }

    /// Parses and validates `cores.lock` text; `what` names it in errors.
    #[staticmethod]
    fn parse(text: &str, what: &str) -> errors::Result<Self> {
        Ok(CoresLock {
            lock: gth_config::CoresLock::parse(text, what)?,
        })
    }

    fn to_toml(&self) -> String {
        self.lock.to_toml()
    }

    /// The core names, sorted.
    #[getter]
    fn names(&self) -> Vec<String> {
        self.lock.cores.keys().cloned().collect()
    }

    /// Core `name`'s `repo`, `commit` and `library_version`.
    fn core(&self, name: &str) -> errors::Result<BTreeMap<&'static str, String>> {
        let core = self.lock.core(name)?;
        Ok(BTreeMap::from([
            ("repo", core.repo.clone()),
            ("commit", core.commit.clone()),
            ("library_version", core.library_version.clone()),
        ]))
    }

    /// Core `name`'s pinned SHA-256 per target triple.
    fn artifacts(&self, name: &str) -> errors::Result<BTreeMap<String, String>> {
        Ok(self.lock.core(name)?.artifacts.clone())
    }

    /// The tag of the GitHub release with this lock's builds.
    #[getter]
    fn release_tag(&self) -> String {
        self.lock.release_tag()
    }

    /// This lock with the artifact hashes for `targets` taken from
    /// `<dir>/<target>/<library file>`.
    fn with_builds(
        &self,
        py: Python<'_>,
        dir: PathBuf,
        targets: Vec<String>,
    ) -> errors::Result<Self> {
        let targets: Vec<&str> = targets.iter().map(String::as_str).collect();
        let lock = py.detach(|| self.lock.with_builds(&dir, &targets))?;
        Ok(CoresLock { lock })
    }

    /// Raises `ConfigError` unless `path` is core `name`'s pinned build for `target`.
    fn check_artifact(
        &self,
        py: Python<'_>,
        name: &str,
        target: &str,
        path: PathBuf,
    ) -> errors::Result<()> {
        let core = self.lock.core(name)?;
        Ok(py.detach(|| core.check_artifact(name, target, &path))?)
    }

    /// Raises `ConfigError` unless `reported` is core `name`'s pinned `library_version`.
    fn check_library_version(&self, name: &str, reported: &str) -> errors::Result<()> {
        Ok(self
            .lock
            .core(name)?
            .check_library_version(name, reported)?)
    }
}

/// Core `name`'s library file name for a target triple, `<name>_libretro.<dll|dylib|so>`.
#[pyfunction]
fn library_file(name: &str, target: &str) -> String {
    gth_config::library_file(name, target)
}

/// Core `name`'s release asset name for a target triple.
#[pyfunction]
fn asset_name(name: &str, target: &str) -> String {
    gth_config::asset_name(name, target)
}

/// Loads the core without a game and unloads it (`retro_init`, `retro_get_system_info`,
/// `retro_deinit`), returning its name, version and content extensions. Raises
/// `CoreLoadError` if it doesn't load, or if a core is loaded in this process.
#[pyfunction]
fn probe_core(
    py: Python<'_>,
    core_path: PathBuf,
    system_dir: PathBuf,
) -> errors::Result<BTreeMap<&'static str, String>> {
    let info = py.detach(|| gth_core::Core::probe(&core_path, &system_dir))?;
    Ok(BTreeMap::from([
        ("library_name", info.library_name),
        ("library_version", info.library_version),
        ("valid_extensions", info.valid_extensions),
    ]))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<CoresLock>()?;
    module.add_function(wrap_pyfunction!(library_file, module)?)?;
    module.add_function(wrap_pyfunction!(asset_name, module)?)?;
    module.add_function(wrap_pyfunction!(probe_core, module)?)?;
    Ok(())
}
