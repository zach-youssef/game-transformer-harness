//! `_native.RamSearch`: a `gth_core::RamSearch` (docs/games.md, "RAM search"), a developer
//! tool for finding RAM variables. Each method takes the `Core` it searches.

use std::path::PathBuf;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::core::Core;
use crate::errors;

#[pyclass(module = "gtharness._native")]
pub struct RamSearch {
    search: gth_core::RamSearch,
}

fn value_error(reason: String) -> errors::Error {
    PyValueError::new_err(reason).into()
}

#[pymethods]
impl RamSearch {
    /// Starts a search of `space` ("system_ram", "save_ram", "video_ram", "rtc" or
    /// "memory_map") through `views` ("bcd4", "le2", "be4-swap16", ...).
    #[new]
    fn new(
        py: Python<'_>,
        mut core: PyRefMut<'_, Core>,
        space: &str,
        views: Vec<String>,
    ) -> errors::Result<Self> {
        let space = space.parse().map_err(value_error)?;
        let views = (views.iter())
            .map(|view| view.parse())
            .collect::<Result<Vec<_>, _>>()
            .map_err(value_error)?;
        let core = core.core()?;
        let search = py.detach(|| gth_core::RamSearch::new(core, space, &views))?;
        Ok(RamSearch { search })
    }

    /// Runs the search script at `script_path` from the core's current state, loading its
    /// savestates from the script's directory. Returns the search and the number of
    /// candidates at the start and after each step.
    #[staticmethod]
    fn run_script(
        py: Python<'_>,
        mut core: PyRefMut<'_, Core>,
        script_path: PathBuf,
    ) -> errors::Result<(RamSearch, Vec<usize>)> {
        let script = gth_config::SearchScript::load(&script_path)?;
        let base = script_path.parent().unwrap_or(&script_path).to_owned();
        let core = core.core_not_recording()?;
        let (search, counts) =
            py.detach(|| gth_core::RamSearch::run_script(core, &script, &base))?;
        Ok((RamSearch { search }, counts))
    }

    /// Keeps the candidates whose value relates to the last snapshot by `relation` ("equal",
    /// "changed", "increased", "decreased", or "equals" with `value`), takes a new snapshot
    /// and returns how many are left.
    fn filter(
        &mut self,
        py: Python<'_>,
        mut core: PyRefMut<'_, Core>,
        relation: &str,
        value: Option<i64>,
    ) -> errors::Result<usize> {
        let relation = gth_config::Relation::new(relation, value).map_err(value_error)?;
        let core = core.core()?;
        let search = &mut self.search;
        Ok(py.detach(|| search.filter(core, relation))?)
    }

    /// Takes a new snapshot without comparing, dropping only the candidates that no longer
    /// decode, and returns how many are left.
    fn rebase(&mut self, py: Python<'_>, mut core: PyRefMut<'_, Core>) -> errors::Result<usize> {
        let core = core.core()?;
        let search = &mut self.search;
        Ok(py.detach(|| search.rebase(core))?)
    }

    /// The first `limit` candidates as dicts with `view`, `memory` and `offset` (or `address`
    /// in the memory map), `size`, `encoding`, `swap16` and `value` at the last snapshot.
    fn candidates<'py>(&self, py: Python<'py>, limit: usize) -> PyResult<Vec<Bound<'py, PyDict>>> {
        (self.search.candidates().take(limit))
            .map(|candidate| {
                let dict = PyDict::new(py);
                dict.set_item("view", candidate.view.to_string())?;
                match candidate.location {
                    gth_config::Location::Memory { memory, offset } => {
                        dict.set_item("memory", memory.name())?;
                        dict.set_item("offset", offset)?;
                    }
                    gth_config::Location::Address(address) => {
                        dict.set_item("address", address)?;
                    }
                }
                dict.set_item("size", candidate.view.size)?;
                dict.set_item("encoding", candidate.view.encoding.name())?;
                dict.set_item("swap16", candidate.view.swap16)?;
                dict.set_item("value", candidate.value)?;
                Ok(dict)
            })
            .collect()
    }

    /// Runs each `(buttons, frames)` input and returns, after every frame, the values of the
    /// first `limit` candidates (None where one doesn't decode). Doesn't filter or snapshot.
    fn watch(
        &self,
        py: Python<'_>,
        mut core: PyRefMut<'_, Core>,
        inputs: Vec<(u16, u32)>,
        limit: usize,
    ) -> errors::Result<Vec<Vec<Option<i64>>>> {
        let core = core.core_not_recording()?;
        let search = &self.search;
        Ok(py.detach(|| search.watch(core, &inputs, limit))?)
    }

    #[getter]
    fn space(&self) -> String {
        self.search.space().to_string()
    }

    #[getter]
    fn views(&self) -> Vec<String> {
        self.search
            .views()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn __len__(&self) -> usize {
        self.search.len()
    }
}

/// Every distinct view's name, as `views = ["all"]` in a script expands.
#[pyfunction]
fn ram_search_views() -> Vec<String> {
    gth_config::View::ALL
        .iter()
        .map(ToString::to_string)
        .collect()
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<RamSearch>()?;
    module.add_function(wrap_pyfunction!(ram_search_views, module)?)?;
    Ok(())
}
