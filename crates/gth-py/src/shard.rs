//! `_native.export_shard`, `_native.write_dataset_shard`, `_native.decode_frames` and
//! `_native.decode_sprites`: training shards (docs/recording-format.md, "Training shards"),
//! written by `gth_record::export` (or `gth_record::write_dataset` for imported datasets) and
//! read back by `gtharness.shards`.

use std::num::NonZeroU32;
use std::path::PathBuf;

use numpy::{
    PyArray4, PyReadonlyArray1, PyReadonlyArray2, PyReadonlyArray4, PyUntypedArrayMethods,
};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use crate::core::{color, filter as interpolation_filter, rotation};
use crate::errors;
use crate::frame::{frames_buffer, into_frames_array};

/// Replays the recording in `recording_dir` on its own core (one per process; the arguments
/// before `out_path` are `_native.Replayer`'s) and writes its shard to `out_path`, replacing
/// any file there. `frameskip` takes an observation every that many frames, or at the
/// recording's own step ends when `None`; `maxpool` makes each observation the maximum of
/// its step's last two frames and `grayscale` gives one gray channel; `filter` is `"all"` or `"playing"`; `sprites` writes
/// every frame's sprite list, raising `ConfigError` with the reason when the game or core has
/// none. Returns the export's counts.
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
    maxpool: bool,
    grayscale: bool,
    frameskip: Option<u32>,
    filter: &str,
    chunk_samples: u32,
    sprites: bool,
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
        maxpool,
        color: color(grayscale),
        steps,
        chunk_samples,
        sprites,
        harness: gth_build_info::build_info_owned(),
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
    dict.set_item("sprites", summary.sprites)?;
    dict.set_item("bytes", summary.bytes)?;
    Ok(dict)
}

/// Writes an imported dataset's samples to the shard `out_path`, replacing any file there:
/// `meta_json` is the `gth_record::DatasetMeta` (docs/recording-format.md, "Imported
/// datasets"), the arrays are C-contiguous numpy arrays of `n` samples (`frames`
/// `uint8[n, height, width, channels]`, `ram` `int64[n, len(ram_names)]`). Returns the
/// write's counts. Raises `ConfigError` if the arrays don't fit the meta or each other.
#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn write_dataset_shard<'py>(
    py: Python<'py>,
    out_path: PathBuf,
    meta_json: &str,
    frames: PyReadonlyArray4<'py, u8>,
    direction: PyReadonlyArray1<'py, u8>,
    buttons: PyReadonlyArray1<'py, u8>,
    ram: PyReadonlyArray2<'py, i64>,
    episode_frame: PyReadonlyArray1<'py, u64>,
    segment: PyReadonlyArray1<'py, u32>,
    chunk_samples: u32,
) -> errors::Result<Bound<'py, PyDict>> {
    let chunk_samples = NonZeroU32::new(chunk_samples)
        .ok_or_else(|| PyValueError::new_err("chunk_samples must be positive"))?;
    let samples = gth_record::DatasetSamples {
        frames: contiguous("frames", frames.as_slice())?,
        direction: contiguous("direction", direction.as_slice())?,
        buttons: contiguous("buttons", buttons.as_slice())?,
        ram: contiguous("ram", ram.as_slice())?,
        episode_frame: contiguous("episode_frame", episode_frame.as_slice())?,
        segment: contiguous("segment", segment.as_slice())?,
    };
    let harness = gth_build_info::build_info_owned();
    let frame_shape = [frames.shape()[1], frames.shape()[2], frames.shape()[3]];
    let summary = py.detach(|| {
        let meta = gth_record::DatasetMeta::from_json(meta_json, &out_path)?;
        let channels = if meta.grayscale { 1 } else { 3 };
        let expected = [meta.obs_size[0], meta.obs_size[1], channels];
        if frame_shape != expected {
            return Err(gth_record::Error::Mismatch {
                path: out_path.clone(),
                reason: format!(
                    "frames are {frame_shape:?} per sample, not the meta's {expected:?}"
                ),
            });
        }
        gth_record::write_dataset(&meta, &samples, chunk_samples, &harness, &out_path)
    })?;
    let dict = PyDict::new(py);
    dict.set_item("path", summary.path)?;
    dict.set_item("samples", summary.samples)?;
    dict.set_item("frame_chunks", summary.frame_chunks)?;
    dict.set_item("bytes", summary.bytes)?;
    Ok(dict)
}

/// A numpy array's data, raising `ValueError` naming the array when it isn't C-contiguous.
fn contiguous<'a, T>(name: &str, slice: Result<&'a [T], numpy::AsSliceError>) -> PyResult<&'a [T]> {
    slice.map_err(|_| PyValueError::new_err(format!("{name} must be C-contiguous")))
}

/// Decodes a frame chunk of the shard at `path` (named in errors) into a new
/// `uint8[samples, height, width, channels]` array (`channels` from the shard's meta: 3 for
/// RGB, 1 for gray). Raises `HarnessError` if the chunk is corrupt or
/// holds another number of bytes.
#[pyfunction]
fn decode_frames<'py>(
    py: Python<'py>,
    data: &[u8],
    samples: usize,
    height: usize,
    width: usize,
    channels: usize,
    path: PathBuf,
) -> errors::Result<Bound<'py, PyArray4<u8>>> {
    if channels != 1 && channels != gth_frame::RGB8_BYTES_PER_PIXEL {
        return Err(
            PyValueError::new_err(format!("channels must be 1 or 3, not {channels}")).into(),
        );
    }
    let buffer = py.detach(|| {
        let mut buffer = frames_buffer(samples, height, width, channels)?;
        gth_record::decode_frames(data, &mut buffer, &path)?;
        Ok::<_, errors::Error>(buffer)
    })?;
    Ok(into_frames_array(
        py, buffer, samples, height, width, channels,
    ))
}

/// Decodes a sprite chunk of the shard at `path` holding `records` records into their bytes
/// (18 each, docs/recording-format.md, "Sprite labels"), which `gtharness.shards` views as a
/// structured array. Raises `HarnessError` if the chunk is corrupt or holds another number of
/// bytes.
#[pyfunction]
fn decode_sprites<'py>(
    py: Python<'py>,
    data: &[u8],
    records: usize,
    path: PathBuf,
) -> errors::Result<Bound<'py, PyBytes>> {
    let bytes = py.detach(|| gth_record::decode_sprites(data, records, &path))?;
    Ok(PyBytes::new(py, &bytes))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(export_shard, module)?)?;
    module.add_function(wrap_pyfunction!(write_dataset_shard, module)?)?;
    module.add_function(wrap_pyfunction!(decode_frames, module)?)?;
    module.add_function(wrap_pyfunction!(decode_sprites, module)?)?;
    Ok(())
}
