//! `_native.Replayer`: a `gth_record::Replayer` with an observation pipeline, so a recording's
//! frames are re-rendered at any size, crop and rotation (docs/recording-format.md).

use std::path::PathBuf;

use numpy::PyArray3;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::config::GameConfig;
use crate::core::{pipeline, render};
use crate::errors;

/// The replayer (`None` once closed, which unloads its core) and its observation pipeline.
#[pyclass(module = "gtharness._native")]
pub struct Replayer {
    replayer: Option<gth_record::Replayer>,
    pipeline: gth_frame::Pipeline,
}

impl Replayer {
    fn replayer(&self) -> errors::Result<&gth_record::Replayer> {
        self.replayer
            .as_ref()
            .ok_or_else(|| PyValueError::new_err("the replayer is closed").into())
    }

    fn replayer_mut(&mut self) -> errors::Result<&mut gth_record::Replayer> {
        self.replayer
            .as_mut()
            .ok_or_else(|| PyValueError::new_err("the replayer is closed").into())
    }
}

#[pymethods]
impl Replayer {
    /// Opens the recording in `recording_dir` and loads its core (one per process) with the
    /// recording's core options and `content_path`, from its start state. The observation
    /// arguments are `_native.Core`'s. Raises `ConfigError` if the core build or the content
    /// differ from the recording's.
    #[new]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        recording_dir: PathBuf,
        core_path: PathBuf,
        content_path: Option<PathBuf>,
        system_dir: PathBuf,
        obs_height: usize,
        obs_width: usize,
        crop: Option<(usize, usize, usize, usize)>,
        rotate: u32,
        interpolation: &str,
    ) -> errors::Result<Self> {
        let pipeline = pipeline(obs_height, obs_width, crop, rotate, interpolation)?;
        let replayer = py.detach(|| {
            let recording = gth_record::Recording::open(&recording_dir)?;
            gth_record::Replayer::load(recording, &core_path, content_path.as_deref(), &system_dir)
        })?;
        Ok(Replayer {
            replayer: Some(replayer),
            pipeline,
        })
    }

    /// Replays the next agent step and returns its joypad mask, its frame count and its last
    /// frame as an observation, or `None` at the end. Raises `DesyncError` at the first check
    /// that differs.
    #[allow(clippy::type_complexity)]
    fn step<'py>(
        mut slf: PyRefMut<'py, Self>,
    ) -> errors::Result<Option<(u16, u32, Bound<'py, PyArray3<u8>>)>> {
        let py = slf.py();
        let this = &mut *slf;
        let replayer = this.replayer_mut()?;
        let Some((buttons, frames)) = py.detach(|| replayer.step())? else {
            return Ok(None);
        };
        let core = this.replayer.as_ref().expect("checked above").core();
        let observation = render(py, core, &mut this.pipeline)?;
        Ok(Some((buttons, frames, observation)))
    }

    /// Replays the rest without rendering, verifying every check; returns the frames replayed
    /// in all.
    fn run_to_end(&mut self, py: Python<'_>) -> errors::Result<u64> {
        let replayer = self.replayer_mut()?;
        Ok(py.detach(|| replayer.run_to_end())?)
    }

    /// Frames replayed so far.
    #[getter]
    fn frame(&self) -> errors::Result<u64> {
        Ok(self.replayer()?.frame())
    }

    /// Frames in the recording.
    #[getter]
    fn frames(&self) -> errors::Result<u64> {
        Ok(self.replayer()?.recording().meta.frames)
    }

    /// Agent steps in the recording.
    #[getter]
    fn steps(&self) -> errors::Result<u64> {
        Ok(self.replayer()?.recording().meta.steps)
    }

    /// The recording's `meta.toml`.
    #[getter]
    fn meta_toml(&self) -> errors::Result<String> {
        Ok(self.replayer()?.recording().meta.to_toml())
    }

    /// The game config the recording was made with.
    #[getter]
    fn game(&self) -> errors::Result<GameConfig> {
        Ok(GameConfig::new(self.replayer()?.recording().game.clone()))
    }

    /// Unloads the core, so another can be loaded in this process. Idempotent.
    fn close(&mut self) {
        self.replayer = None;
    }

    #[getter]
    fn closed(&self) -> bool {
        self.replayer.is_none()
    }
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Replayer>()?;
    Ok(())
}
