//! `_native.Core`: a `gth_core::Core` with the `gth_frame::Pipeline` that turns its frames
//! into observations, and the `gth_record::Recorder` it runs through while recording. One
//! call per agent step; the frames in between never reach Python.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::PathBuf;

use numpy::PyArray3;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::config::GameConfig;
use crate::errors;
use crate::frame::{into_rgb8_array, rgb8_buffer};

/// The loaded core (`None` once closed), its observation pipeline and the recording in
/// progress, if any.
#[pyclass(module = "gtharness._native")]
pub struct Core {
    core: Option<gth_core::Core>,
    pipeline: gth_frame::Pipeline,
    content_path: Option<PathBuf>,
    recorder: Option<gth_record::Recorder>,
}

fn closed() -> errors::Error {
    PyValueError::new_err("the core is closed").into()
}

/// The observation pipeline of `crop` `(x, y, width, height)`, `rotate` degrees and
/// `interpolation`, as `Core` and `Replayer` take them.
pub(crate) fn pipeline(
    obs_height: usize,
    obs_width: usize,
    crop: Option<(usize, usize, usize, usize)>,
    rotate: u32,
    interpolation: &str,
) -> errors::Result<gth_frame::Pipeline> {
    let crop = crop.map(|(x, y, width, height)| gth_frame::Rect {
        x,
        y,
        width,
        height,
    });
    Ok(gth_frame::Pipeline::new(
        obs_height,
        obs_width,
        crop,
        rotation(rotate)?,
        filter(interpolation)?,
    )?)
}

/// Renders the core's last frame into a new array.
pub(crate) fn render<'py>(
    py: Python<'py>,
    core: &gth_core::Core,
    pipeline: &mut gth_frame::Pipeline,
) -> errors::Result<Bound<'py, PyArray3<u8>>> {
    let (height, width) = (pipeline.out_height(), pipeline.out_width());
    let buffer = py.detach(|| {
        let mut buffer = rgb8_buffer(height, width)?;
        core.with_frame(|frame| pipeline.render(frame, &mut buffer))?;
        Ok::<_, errors::Error>(buffer)
    })?;
    Ok(into_rgb8_array(py, buffer, height, width))
}

fn rotation(degrees: u32) -> errors::Result<gth_frame::Rotation> {
    match degrees {
        0 => Ok(gth_frame::Rotation::None),
        90 => Ok(gth_frame::Rotation::Cw90),
        270 => Ok(gth_frame::Rotation::Cw270),
        _ => {
            Err(PyValueError::new_err(format!("rotate must be 0, 90 or 270, not {degrees}")).into())
        }
    }
}

fn filter(name: &str) -> errors::Result<gth_frame::Filter> {
    match name {
        "area" => Ok(gth_frame::Filter::Area),
        "bilinear" => Ok(gth_frame::Filter::Bilinear),
        _ => Err(PyValueError::new_err(format!(
            "interpolation must be \"area\" or \"bilinear\", not {name:?}"
        ))
        .into()),
    }
}

impl Core {
    fn core(&mut self) -> errors::Result<&mut gth_core::Core> {
        self.core.as_mut().ok_or_else(closed)
    }

    /// The core, unless a recording is in progress: loading a state would break it.
    fn core_not_recording(&mut self) -> errors::Result<&mut gth_core::Core> {
        if self.recorder.is_some() {
            return Err(PyValueError::new_err("finish the recording first").into());
        }
        self.core()
    }

    /// Runs each `(buttons, frames)` step, through the recorder while recording.
    fn run(&mut self, py: Python<'_>, inputs: &[(u16, u32)]) -> errors::Result<()> {
        let core = self.core.as_mut().ok_or_else(closed)?;
        let recorder = &mut self.recorder;
        let result = py.detach(|| match recorder {
            Some(recorder) => inputs
                .iter()
                .try_for_each(|&(buttons, frames)| recorder.run(core, frames, buttons)),
            None => {
                core.run_inputs(inputs);
                Ok(())
            }
        });
        if result.is_err() {
            // A failed recorder refuses to go on (gth_record::Error::Failed): drop it, leaving
            // its directory incomplete, so the next episode can record again.
            self.recorder = None;
        }
        Ok(result?)
    }
}

#[pymethods]
impl Core {
    /// Loads a core (one per process) with `core_options` set, and sets up the observation
    /// pipeline. `crop` is `(x, y, width, height)` in the core's pixels, `rotate` is 0, 90 or
    /// 270 degrees clockwise, `interpolation` is "area" or "bilinear".
    #[new]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        core_path: PathBuf,
        content_path: Option<PathBuf>,
        system_dir: PathBuf,
        core_options: BTreeMap<String, String>,
        obs_height: usize,
        obs_width: usize,
        crop: Option<(usize, usize, usize, usize)>,
        rotate: u32,
        interpolation: &str,
    ) -> errors::Result<Self> {
        let pipeline = pipeline(obs_height, obs_width, crop, rotate, interpolation)?;
        let core = py.detach(|| {
            gth_core::Core::load(
                &core_path,
                content_path.as_deref(),
                &system_dir,
                &core_options,
            )
        })?;
        Ok(Core {
            core: Some(core),
            pipeline,
            content_path,
            recorder: None,
        })
    }

    /// Runs `frames` frames with `buttons` held (port 0 joypad bitmask) and returns the last
    /// one as a `uint8[obs_height, obs_width, 3]` observation. While recording, this is one
    /// recorded step.
    fn step<'py>(
        mut slf: PyRefMut<'py, Self>,
        frames: u32,
        buttons: u16,
    ) -> errors::Result<Bound<'py, PyArray3<u8>>> {
        let py = slf.py();
        let this = &mut *slf;
        this.run(py, &[(buttons, frames)])?;
        let core = this.core.as_ref().expect("checked by run");
        render(py, core, &mut this.pipeline)
    }

    /// Runs each `(buttons, frames)` pair in order, such as a reset script; while recording,
    /// each is a recorded step.
    fn run_inputs(&mut self, py: Python<'_>, inputs: Vec<(u16, u32)>) -> errors::Result<()> {
        self.run(py, &inputs)
    }

    /// Starts recording (docs/recording-format.md) into the new directory `directory`, from
    /// the core's current state, until `finish_recording`. `config` is the game the core
    /// runs; `recorder` is "human", "policy", "scripted" or "converted"; a check follows
    /// every `checks_every` frames. Until then, every frame must run through `step` or
    /// `run_inputs`, and `reset` and `unserialize` raise `ValueError`.
    #[allow(clippy::too_many_arguments)]
    fn start_recording(
        &mut self,
        py: Python<'_>,
        directory: PathBuf,
        config: PyRef<'_, GameConfig>,
        game_id: String,
        recorder: &str,
        player: String,
        license: String,
        checks_every: u32,
    ) -> errors::Result<()> {
        let kind = gth_record::RecorderKind::from_name(recorder).ok_or_else(|| {
            let names = gth_record::RecorderKind::ALL.map(gth_record::RecorderKind::name);
            PyValueError::new_err(format!(
                "recorder must be one of {names:?}, not {recorder:?}"
            ))
        })?;
        let checks_every = NonZeroU32::new(checks_every)
            .ok_or_else(|| PyValueError::new_err("checks_every must be at least 1"))?;
        self.core_not_recording()?;
        let info = gth_record::RecordingInfo {
            game_id,
            recorder: kind,
            player,
            license,
            harness: crate::build_info()
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
            // No cores.lock exists until P1 (docs/games.md, "cores.lock").
            core_lock: None,
            checks_every,
        };
        let game = &config.config;
        let content = self.content_path.as_deref();
        let core = self.core.as_mut().ok_or_else(closed)?;
        let recorder =
            py.detach(|| gth_record::Recorder::create(&directory, core, game, content, info))?;
        self.recorder = Some(recorder);
        Ok(())
    }

    /// Completes the recording in progress and returns its directory, or `None` if there is
    /// none.
    fn finish_recording(&mut self, py: Python<'_>) -> errors::Result<Option<PathBuf>> {
        let Some(recorder) = self.recorder.take() else {
            return Ok(None);
        };
        let core = self.core()?;
        Ok(Some(py.detach(|| recorder.finish(core))?))
    }

    /// The directory of the recording in progress, or `None`.
    #[getter]
    fn recording(&self) -> Option<PathBuf> {
        self.recorder
            .as_ref()
            .map(|recorder| recorder.dir().to_owned())
    }

    /// Every RAM variable of `config`, decoded, by name (for rewards and logs, never for the
    /// policy), and whether they end the episode: one call per step for both.
    fn read_ram(
        &mut self,
        config: PyRef<'_, GameConfig>,
    ) -> errors::Result<(BTreeMap<String, i64>, bool)> {
        let ram = self.core()?.read_ram_vars(&config.config.ram)?;
        let terminated = config.config.terminated(&ram)?;
        Ok((ram, terminated))
    }

    /// Raises `HarnessError` if a RAM variable of `config` isn't in the core's memory; after
    /// this, `read_ram` can fail only on bytes that don't decode.
    fn check_ram(&mut self, config: PyRef<'_, GameConfig>) -> errors::Result<()> {
        Ok(self.core()?.check_ram_vars(&config.config.ram)?)
    }

    fn reset(&mut self, py: Python<'_>) -> errors::Result<()> {
        let core = self.core_not_recording()?;
        py.detach(|| core.reset());
        Ok(())
    }

    fn serialize<'py>(&mut self, py: Python<'py>) -> errors::Result<Bound<'py, PyBytes>> {
        let core = self.core()?;
        let state = py.detach(|| core.serialize())?;
        Ok(PyBytes::new(py, &state))
    }

    fn unserialize(&mut self, py: Python<'_>, state: &[u8]) -> errors::Result<()> {
        let core = self.core_not_recording()?;
        py.detach(|| core.unserialize(state))?;
        Ok(())
    }

    /// A copy of the core's memory `id` (`RETRO_MEMORY_*`), or `None` if it has none.
    fn memory<'py>(
        &mut self,
        py: Python<'py>,
        id: u32,
    ) -> errors::Result<Option<Bound<'py, PyBytes>>> {
        Ok(self
            .core()?
            .memory(id)
            .map(|memory| PyBytes::new(py, memory)))
    }

    /// The core's name, version and content extensions, sorted by key.
    fn system_info(&mut self) -> errors::Result<BTreeMap<&'static str, String>> {
        let info = self.core()?.system_info();
        Ok(BTreeMap::from([
            ("library_name", info.library_name.clone()),
            ("library_version", info.library_version.clone()),
            ("valid_extensions", info.valid_extensions.clone()),
        ]))
    }

    /// Geometry and timing: `base_width`, `base_height`, `max_width`, `max_height`,
    /// `aspect_ratio`, `fps`, `sample_rate`.
    fn av_info(&mut self) -> errors::Result<BTreeMap<&'static str, f64>> {
        let av = self.core()?.av_info();
        Ok(BTreeMap::from([
            ("base_width", f64::from(av.base_width)),
            ("base_height", f64::from(av.base_height)),
            ("max_width", f64::from(av.max_width)),
            ("max_height", f64::from(av.max_height)),
            ("aspect_ratio", f64::from(av.aspect_ratio)),
            ("fps", av.fps),
            ("sample_rate", av.sample_rate),
        ]))
    }

    /// Whether `reset` restores the state right after loading (True) or falls back to the
    /// core's own `retro_reset`, a soft reset that may keep RAM (False).
    #[getter]
    fn resets_to_load_state(&mut self) -> errors::Result<bool> {
        Ok(self.core()?.resets_to_load_state())
    }

    /// Frames run since loading.
    #[getter]
    fn frames_run(&mut self) -> errors::Result<u64> {
        Ok(self.core()?.frames_run())
    }

    /// Unloads the core, so another can be loaded in this process. Idempotent. A recording
    /// in progress is left incomplete (no `meta.toml`): call `finish_recording` first.
    fn close(&mut self) {
        self.recorder = None;
        self.core = None;
    }

    #[getter]
    fn closed(&self) -> bool {
        self.core.is_none()
    }
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Core>()?;
    Ok(())
}
