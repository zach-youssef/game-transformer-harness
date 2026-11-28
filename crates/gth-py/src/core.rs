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
use crate::cores::CoresLock;
use crate::errors;
use crate::frame::{frame_buffer, into_frame_array};

/// The loaded core (`None` once closed), its observation pipeline, the recording in
/// progress, if any, and the environment's random numbers (sticky actions, no-ops).
#[pyclass(module = "gtharness._native")]
pub struct Core {
    core: Option<gth_core::Core>,
    pipeline: gth_frame::Pipeline,
    content_path: Option<PathBuf>,
    recorder: Option<gth_record::Recorder>,
    rng: gth_core::Rng,
}

fn closed() -> errors::Error {
    PyValueError::new_err("the core is closed").into()
}

/// The observation pipeline of `crop` `(x, y, width, height)`, `rotate` degrees,
/// `interpolation` and `color`, as `Core` and `Replayer` take them.
pub(crate) fn pipeline(
    obs_height: usize,
    obs_width: usize,
    crop: Option<(usize, usize, usize, usize)>,
    rotate: u32,
    interpolation: &str,
    color: gth_frame::Color,
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
        color,
    )?)
}

/// Renders the core's last frame into a new array, max-pooled with the frame before it when
/// the core keeps that (`gth_core::Core::set_keep_previous_frame`).
pub(crate) fn render<'py>(
    py: Python<'py>,
    core: &gth_core::Core,
    pipeline: &mut gth_frame::Pipeline,
) -> errors::Result<Bound<'py, PyArray3<u8>>> {
    let (height, width) = (pipeline.out_height(), pipeline.out_width());
    let channels = pipeline.color().channels();
    let buffer = py.detach(|| {
        let mut buffer = frame_buffer(height, width, channels)?;
        core.with_frames(|frame, previous| match previous {
            Some(previous) => pipeline.render_max(frame, previous, &mut buffer),
            None => pipeline.render(frame, &mut buffer),
        })?;
        Ok::<_, errors::Error>(buffer)
    })?;
    Ok(into_frame_array(py, buffer, height, width, channels))
}

/// `Color::Gray` for `grayscale`, else `Color::Rgb`.
pub(crate) fn color(grayscale: bool) -> gth_frame::Color {
    if grayscale {
        gth_frame::Color::Gray
    } else {
        gth_frame::Color::Rgb
    }
}

pub(crate) fn rotation(degrees: u32) -> errors::Result<gth_frame::Rotation> {
    match degrees {
        0 => Ok(gth_frame::Rotation::None),
        90 => Ok(gth_frame::Rotation::Cw90),
        270 => Ok(gth_frame::Rotation::Cw270),
        _ => {
            Err(PyValueError::new_err(format!("rotate must be 0, 90 or 270, not {degrees}")).into())
        }
    }
}

pub(crate) fn filter(name: &str) -> errors::Result<gth_frame::Filter> {
    match name {
        "area" => Ok(gth_frame::Filter::Area),
        "bilinear" => Ok(gth_frame::Filter::Bilinear),
        "bicubic" => Ok(gth_frame::Filter::Bicubic),
        _ => Err(PyValueError::new_err(format!(
            "interpolation must be \"area\", \"bilinear\" or \"bicubic\", not {name:?}"
        ))
        .into()),
    }
}

impl Core {
    pub(crate) fn core(&mut self) -> errors::Result<&mut gth_core::Core> {
        self.core.as_mut().ok_or_else(closed)
    }

    /// The core, unless a recording is in progress: loading a state, or running frames outside
    /// the recorder, would break it.
    pub(crate) fn core_not_recording(&mut self) -> errors::Result<&mut gth_core::Core> {
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
        self.after_run(result)
    }

    /// Runs one sticky step (`gth_core::Core::run_sticky`), through the recorder while
    /// recording.
    fn run_sticky(
        &mut self,
        py: Python<'_>,
        frames: u32,
        buttons: u16,
        sticky_prob: f64,
    ) -> errors::Result<()> {
        let core = self.core.as_mut().ok_or_else(closed)?;
        let (recorder, rng) = (&mut self.recorder, &mut self.rng);
        let result = py.detach(|| match recorder {
            Some(recorder) => recorder.run_sticky(core, frames, buttons, sticky_prob, rng),
            None => {
                core.run_sticky(frames, buttons, sticky_prob, rng);
                Ok(())
            }
        });
        self.after_run(result)
    }

    fn after_run(&mut self, result: gth_record::Result<()>) -> errors::Result<()> {
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
    /// Loads a core (one per process) with `core_options` set, settling `settle_frames`
    /// after each savestate load (a game's `settle_frames`, docs/games.md), and sets up the
    /// observation pipeline. `crop` is `(x, y, width, height)` in the core's pixels, `rotate` is
    /// 0, 90 or 270 degrees clockwise, `interpolation` is "area", "bilinear" or "bicubic".
    /// `maxpool` makes each observation the per-channel maximum of a step's last two frames
    /// (settable later, `maxpool`); `grayscale` makes observations one channel of BT.601
    /// luma, `uint8[obs_height, obs_width, 1]`.
    #[new]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        core_path: PathBuf,
        content_path: Option<PathBuf>,
        system_dir: PathBuf,
        core_options: BTreeMap<String, String>,
        settle_frames: u32,
        obs_height: usize,
        obs_width: usize,
        crop: Option<(usize, usize, usize, usize)>,
        rotate: u32,
        interpolation: &str,
        maxpool: bool,
        grayscale: bool,
    ) -> errors::Result<Self> {
        let pipeline = pipeline(
            obs_height,
            obs_width,
            crop,
            rotate,
            interpolation,
            color(grayscale),
        )?;
        let mut core = py.detach(|| {
            gth_core::Core::load(
                &core_path,
                content_path.as_deref(),
                &system_dir,
                &core_options,
                settle_frames,
            )
        })?;
        core.set_keep_previous_frame(maxpool);
        Ok(Core {
            core: Some(core),
            pipeline,
            content_path,
            recorder: None,
            rng: gth_core::Rng::new(0),
        })
    }

    /// Runs `frames` frames with `buttons` held (port 0 joypad bitmask), each frame keeping
    /// the previous frame's buttons instead with probability `sticky_prob` (sticky actions,
    /// drawn from the core's RNG, `seed`), and returns the last one (max-pooled with the one
    /// before it under `maxpool`) as a `uint8[obs_height, obs_width, channels]` observation.
    /// While recording, this is one recorded step of the buttons each frame ran with.
    fn step<'py>(
        mut slf: PyRefMut<'py, Self>,
        frames: u32,
        buttons: u16,
        sticky_prob: f64,
    ) -> errors::Result<Bound<'py, PyArray3<u8>>> {
        let py = slf.py();
        if !(0.0..1.0).contains(&sticky_prob) {
            return Err(PyValueError::new_err(format!(
                "sticky_prob must be in [0, 1), not {sticky_prob}"
            ))
            .into());
        }
        let this = &mut *slf;
        this.run_sticky(py, frames, buttons, sticky_prob)?;
        let core = this.core.as_ref().expect("checked by run");
        render(py, core, &mut this.pipeline)
    }

    /// Whether observations are the maximum of a step's last two frames. Setting it keeps
    /// (or drops) the previous frame from then on.
    #[getter]
    fn get_maxpool(&self) -> errors::Result<bool> {
        Ok(self
            .core
            .as_ref()
            .ok_or_else(closed)?
            .keeps_previous_frame())
    }

    #[setter]
    fn set_maxpool(&mut self, maxpool: bool) -> errors::Result<()> {
        self.core()?.set_keep_previous_frame(maxpool);
        Ok(())
    }

    /// Bytes per observation pixel: 1 under `grayscale`, else 3.
    #[getter]
    fn channels(&self) -> usize {
        self.pipeline.color().channels()
    }

    /// Runs each `(buttons, frames)` pair in order, such as a reset script; while recording,
    /// each is a recorded step.
    fn run_inputs(&mut self, py: Python<'_>, inputs: Vec<(u16, u32)>) -> errors::Result<()> {
        self.run(py, &inputs)
    }

    /// Seeds the RNG behind sticky actions and `random_below` (0 until seeded). It runs on
    /// across resets and savestate loads until it is seeded again.
    fn seed(&mut self, seed: u64) {
        self.rng = gth_core::Rng::new(seed);
    }

    /// A uniform integer in `0..n` from the RNG (`seed`), such as a no-op start's length.
    fn random_below(&mut self, n: u64) -> errors::Result<u64> {
        if n == 0 {
            return Err(PyValueError::new_err("random_below needs n >= 1").into());
        }
        Ok(self.rng.below(n))
    }

    /// Starts recording (docs/recording-format.md) into the new directory `directory`, from
    /// the core's current state, until `finish_recording`. `config` is the game the core
    /// runs; `core_lock` is a lock that pins this core's build, whose entry for the game's
    /// core goes into the recording (raises `ConfigError` if it has none), or None;
    /// `recorder` is "human", "policy", "scripted" or "converted"; a check follows every
    /// `checks_every` frames. Until then, every frame must run through `step` or
    /// `run_inputs`, and `reset` and `unserialize` raise `ValueError`.
    #[allow(clippy::too_many_arguments)]
    fn start_recording(
        &mut self,
        py: Python<'_>,
        directory: PathBuf,
        config: PyRef<'_, GameConfig>,
        game_id: String,
        core_lock: Option<PyRef<'_, CoresLock>>,
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
        let core_lock = match core_lock {
            Some(lock) => Some(lock.lock.core(&config.config.core)?.clone()),
            None => None,
        };
        let info = gth_record::RecordingInfo {
            game_id,
            recorder: kind,
            player,
            license,
            harness: gth_build_info::build_info_owned(),
            core_lock,
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
