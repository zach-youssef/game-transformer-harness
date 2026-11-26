//! `_native.export_shard` and `_native.decode_frames`: training shards (docs/recording-format.md,
//! "Training shards"), written by `gth_record::export` and read back by `gtharness.shards`.

use std::num::NonZeroU32;
use std::path::PathBuf;

use numpy::PyArray4;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::core::{filter as interpolation_filter, rotation};
use crate::errors;
use crate::frame::{into_rgb8_frames, rgb8_frames_buffer};

/// Replays the recording in `recording_dir` on its own core (one per process; the arguments
/// before `out_path` are `_native.Replayer`'s) and writes its shard to `out_path`, replacing
/// any file there. `frameskip` takes an observation every that many frames, or at the
/// recording's own step ends when `None`; `filter` is `"all"` or `"playing"`. Returns the export's counts.
/// Raises `DesyncError` if the replay desyncs, leaving no shard.
#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn export_shard<'py>(
    py: Python<'py>,
    recording_dir: PathBuf,
    core_path: PathBuf,
    content_path: Option<PathBuf>,
    system_dir: PathBuf,
    out_path: PathBuf,
    obs_height: usize,
    obs_width: usize,
    crop: Option<(usize, usize, usize, usize)>,
    rotate: u32,
    interpolation: &str,
    frameskip: Option<u32>,
    filter: &str,
    chunk_samples: u32,
) -> errors::Result<Bound<'py, PyDict>> {
    let steps = match frameskip {
        None => gth_record::Steps::Recorded,
        Some(k) => gth_record::Steps::Every(
            NonZeroU32::new(k)
                .ok_or_else(|| PyValueError::new_err("frameskip must be positive"))?,
        ),
    };
    let chunk_samples = NonZeroU32::new(chunk_samples)
        .ok_or_else(|| PyValueError::new_err("chunk_samples must be positive"))?;
    let mut sample_filter = gth_record::filter_by_name(filter).ok_or_else(|| {
        PyValueError::new_err(format!(
            "filter must be \"all\" or \"playing\", not {filter:?}"
        ))
    })?;
    let options = gth_record::ShardOptions {
        obs_height,
        obs_width,
        crop: crop.map(|(x, y, width, height)| gth_frame::Rect {
            x,
            y,
            width,
            height,
        }),
        rotation: rotation(rotate)?,
        interpolation: interpolation_filter(interpolation)?,
        steps,
        chunk_samples,
        harness: crate::build_info()
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
    };
    let summary = py.detach(|| {
        let recording = gth_record::Recording::open(&recording_dir)?;
        let mut replayer = gth_record::Replayer::load(
            recording,
            &core_path,
            content_path.as_deref(),
            &system_dir,
        )?;
        gth_record::export(&mut replayer, &options, sample_filter.as_mut(), &out_path)
    })?;
    let dict = PyDict::new(py);
    dict.set_item("path", summary.path)?;
    dict.set_item("samples", summary.samples)?;
    dict.set_item("frames", summary.frames)?;
    dict.set_item("unlabelled", summary.unlabelled)?;
    dict.set_item("filtered", summary.filtered)?;
    dict.set_item("frame_chunks", summary.frame_chunks)?;
    dict.set_item("bytes", summary.bytes)?;
    Ok(dict)
}

/// Decodes a frame chunk of the shard at `path` (named in errors) into a new
/// `uint8[samples, height, width, 3]` array. Raises `HarnessError` if the chunk is corrupt or
/// holds another number of bytes.
#[pyfunction]
fn decode_frames<'py>(
    py: Python<'py>,
    data: &[u8],
    samples: usize,
    height: usize,
    width: usize,
    path: PathBuf,
) -> errors::Result<Bound<'py, PyArray4<u8>>> {
    let buffer = py.detach(|| {
        let mut buffer = rgb8_frames_buffer(samples, height, width)?;
        gth_record::decode_frames(data, &mut buffer, &path)?;
        Ok::<_, errors::Error>(buffer)
    })?;
    Ok(into_rgb8_frames(py, buffer, samples, height, width))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(export_shard, module)?)?;
    module.add_function(wrap_pyfunction!(decode_frames, module)?)?;
    Ok(())
}
