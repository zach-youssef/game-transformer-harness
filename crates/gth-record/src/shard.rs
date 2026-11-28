//! Training shards (docs/recording-format.md, "Training shards"): a recording re-rendered by
//! replay at the model's observation size and rate. Each sample is an observation and the
//! action the player chose after seeing it; every replayed frame's action is kept too. A shard
//! is one `.npz` file: a zip of uncompressed members that numpy's `np.load` opens, holding
//! the labels as `.npy` arrays, the frames as zstd chunks and `meta.json`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use gth_config::{Action, ActionSpace, Direction};
use gth_frame::{Color, Filter, Pipeline, Rect, Rotation};
use gth_sprites::{Sprite, SpriteStream};
use rawzip::ZipArchiveWriter;
use serde::{Deserialize, Serialize};

use crate::meta::Meta;
use crate::replay::Replayer;
use crate::{Error, Result, format_error, io_error};

/// The shard format version this build writes (`meta.json`'s `shard_version`).
pub const SHARD_VERSION: u32 = 3;

/// zstd level of the frame chunks. Frames are written once per export and read every epoch,
/// so decompression speed (the same at every level) matters most; level 3 keeps compression
/// well ahead of replay.
pub const FRAME_ZSTD_LEVEL: i32 = 3;

/// Replayed frames per sprite chunk (`sprites_00000.zst`, ...): about 4096 x 100 records of
/// 18 bytes on a busy Psikyo screen, 7 MB raw, so a reader decodes a minute of play at once.
pub const SPRITE_CHUNK_FRAMES: u32 = 4096;

/// Where observations are taken: at the recording's own step ends, or every `k` frames from
/// the start of the recording (the model's rate, independent of the rate it was recorded at).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Steps {
    Recorded,
    Every(NonZeroU32),
}

/// What a [`SampleFilter`] sees of one sample: an observation and the action after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepInfo {
    /// The action held during the first frame after the observation, or `None` when that
    /// frame's mask holds a button no slot maps (the sample is then never kept).
    pub action: Option<Action>,
    /// Whether the game's `episode.terminate` condition holds at the observation.
    pub terminated: bool,
    /// Frames replayed since the start state, at the observation.
    pub episode_frame: u64,
}

/// Decides which samples a shard keeps, in order, one call per observation that has a
/// following frame. Unlabelled samples reach it too, so it sees every terminate, but are
/// dropped whatever it answers.
pub trait SampleFilter {
    /// The name recorded in `meta.json`.
    fn name(&self) -> &str;
    fn keep(&mut self, step: &StepInfo) -> bool;
}

/// Keeps every labelled sample.
#[derive(Debug, Default)]
pub struct KeepAll;

impl SampleFilter for KeepAll {
    fn name(&self) -> &str {
        "all"
    }

    fn keep(&mut self, _: &StepInfo) -> bool {
        true
    }
}

/// Keeps the samples from the observation before the first non-neutral action (where the
/// player decided to start) through game over, and drops the rest: the attract mode or idle
/// start before play, and anything after the game ended. Game over is the first observation
/// where `episode.terminate` holds after it has not held at least once from the start (the
/// kept observation before the first input included), so a title screen or attract mode that
/// looks like game over (`lives == 0` until the game sets them) doesn't end the episode at
/// once; the game-over observation is kept.
#[derive(Debug, Default)]
pub struct Playing {
    started: bool,
    armed: bool,
    over: bool,
}

impl SampleFilter for Playing {
    fn name(&self) -> &str {
        "playing"
    }

    fn keep(&mut self, step: &StepInfo) -> bool {
        if self.over {
            return false;
        }
        if !self.started {
            let input = step
                .action
                .is_some_and(|a| a.direction != Direction::Neutral || a.buttons != 0);
            // The observation the first input answers is kept; a terminate there isn't game
            // over, but its absence arms it.
            self.started = input;
            self.armed = input && !step.terminated;
            return input;
        }
        if !step.terminated {
            self.armed = true;
        } else if self.armed {
            self.over = true;
        }
        true
    }
}

/// The built-in filter called `name` (`all` or `playing`).
pub fn filter_by_name(name: &str) -> Option<Box<dyn SampleFilter + Send>> {
    match name {
        "all" => Some(Box::new(KeepAll)),
        "playing" => Some(Box::new(Playing::default())),
        _ => None,
    }
}

/// How a recording is rendered into a shard. Every field is a choice the consumer makes;
/// none has a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardOptions {
    pub obs_height: usize,
    pub obs_width: usize,
    /// The source rectangle, or `None` for the whole frame.
    pub crop: Option<Rect>,
    pub rotation: Rotation,
    pub interpolation: Filter,
    /// Whether each observation is the per-channel maximum of its step's last two frames
    /// (`gth_core::Core::set_keep_previous_frame`).
    pub maxpool: bool,
    /// RGB or one gray channel per pixel.
    pub color: Color,
    pub steps: Steps,
    /// Samples per zstd frame chunk: larger chunks compress better, smaller ones let a reader
    /// decode less to reach one sample.
    pub chunk_samples: NonZeroU32,
    /// Whether to write every replayed frame's sprite list (docs/recording-format.md, "Sprite
    /// labels"). The export fails, saying why, if the game has no `sprite_hw` or the core
    /// can't give its sprite list.
    pub sprites: bool,
    /// The exporting harness build (`_native.build_info()`).
    pub harness: BTreeMap<String, String>,
}

/// `meta.json`: what a shard holds and how it was made (docs/recording-format.md).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShardMeta {
    pub shard_version: u32,
    /// The recording's directory name (or a video's id).
    pub source_id: String,
    /// The episode's index in its source; a recording holds one episode, so 0.
    pub episode_id: u32,
    pub game_id: String,
    pub fps: f64,
    /// Frames per sample, or `None` when it varies (`episode_frame` gives the step ends).
    pub frameskip: Option<u32>,
    /// `[height, width]`.
    pub obs_size: [usize; 2],
    /// `[x, y, width, height]` in source pixels, or `None` for the whole frame.
    pub crop: Option<[usize; 4]>,
    /// Degrees clockwise: 0, 90 or 270.
    pub orientation: u32,
    /// `area`, `bilinear` or `bicubic`.
    pub interpolation: String,
    /// Whether each observation is the maximum of its step's last two frames; `false` before
    /// version 3.
    #[serde(default)]
    pub maxpool: bool,
    /// Whether observations are BT.601 gray; `false` before version 3.
    #[serde(default)]
    pub grayscale: bool,
    /// Bytes per observation pixel: 3 (RGB), or 1 (`grayscale`); 3 before version 3.
    #[serde(default = "rgb_channels")]
    pub channels: u32,
    /// The names of `ram.npy`'s columns.
    pub ram_names: Vec<String>,
    /// `replay` (from a recording) or `idm` (labelled by an inverse dynamics model).
    pub label_source: String,
    /// The IDM's id for `idm` labels.
    pub idm_model: Option<String>,
    /// The filter that chose the samples ([`SampleFilter::name`]).
    pub filter: String,
    pub samples: u64,
    /// Frames replayed: the length of the per-frame arrays.
    pub frames: u64,
    /// Samples per frame chunk; the last chunk holds the rest.
    pub chunk_samples: u32,
    pub frame_chunks: u32,
    /// Whether `episode.terminate` holds at the last sample's observation: the shard ends at
    /// game over.
    pub terminated: bool,
    /// The board whose sprite lists the shard holds (`game.toml`'s `sprite_hw`), or `None`
    /// when it holds none (and the other `sprite_*` fields are `None` too).
    #[serde(default)]
    pub sprite_hw: Option<String>,
    /// `[width, height]` of the native screen the sprite coordinates are in: before `crop`,
    /// `orientation` and the resize.
    #[serde(default)]
    pub sprite_screen: Option<[u32; 2]>,
    /// Replayed frames per sprite chunk; the last chunk holds the rest.
    #[serde(default)]
    pub sprite_chunk_frames: Option<u32>,
    #[serde(default)]
    pub sprite_chunks: Option<u32>,
    /// The recording's `meta.toml`; for `idm` shards, `None`.
    pub recording: Option<Meta>,
    /// The exporting harness build.
    pub harness: BTreeMap<String, String>,
}

fn rgb_channels() -> u32 {
    Color::Rgb.channels() as u32
}

/// What an export did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardSummary {
    pub path: PathBuf,
    pub samples: u64,
    /// Frames replayed.
    pub frames: u64,
    /// Samples dropped because the frame after the observation holds a button no slot maps.
    pub unlabelled: u64,
    /// Labelled samples the filter dropped.
    pub filtered: u64,
    pub frame_chunks: u32,
    /// Sprite records written, over every frame (0 without sprites).
    pub sprites: u64,
    /// The shard's size in bytes.
    pub bytes: u64,
}

/// The member holding frame chunk `index`.
pub fn chunk_name(index: u32) -> String {
    format!("frames_{index:05}.zst")
}

/// The member holding sprite chunk `index`.
pub fn sprite_chunk_name(index: u32) -> String {
    format!("sprites_{index:05}.zst")
}

/// Replays `replayer`'s recording from its start state to the end and writes the shard to
/// `out`, through `out` with `.partial` appended, renamed when complete, so a shard that
/// exists is whole. Fails with [`Error::Desync`] if the replay desyncs, and with
/// [`Error::Mismatch`] if the options don't fit the core's frames.
pub fn export(
    replayer: &mut Replayer,
    options: &ShardOptions,
    filter: &mut dyn SampleFilter,
    out: &Path,
) -> Result<ShardSummary> {
    let mut partial = out.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let result = write_shard(replayer, options, filter, &partial).and_then(|summary| {
        std::fs::rename(&partial, out).map_err(io_error(out))?;
        Ok(ShardSummary {
            path: out.to_owned(),
            ..summary
        })
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

fn write_shard(
    replayer: &mut Replayer,
    options: &ShardOptions,
    filter: &mut dyn SampleFilter,
    path: &Path,
) -> Result<ShardSummary> {
    if replayer.frame() != 0 {
        return Err(mismatch(replayer, "the replay has already started".into()));
    }
    let mut pipeline = Pipeline::new(
        options.obs_height,
        options.obs_width,
        options.crop,
        options.rotation,
        options.interpolation,
        options.color,
    )
    .map_err(|error| mismatch(replayer, error.to_string()))?;
    replayer.core_mut().set_keep_previous_frame(options.maxpool);
    let frame_len = pipeline.out_len();
    let chunk_samples = options.chunk_samples.get() as usize;
    let chunk_len = frame_len
        .checked_mul(chunk_samples)
        .filter(|&len| isize::try_from(len).is_ok())
        .ok_or_else(|| mismatch(replayer, format!("{chunk_samples} samples per chunk")))?;
    let mut chunk = Vec::new();
    chunk
        .try_reserve_exact(chunk_len)
        .map_err(|error| mismatch(replayer, format!("a chunk of {chunk_len} bytes: {error}")))?;

    let io = |source: io::Error| io_error(path)(source);
    let file = File::create(path).map_err(io_error(path))?;
    let mut zip = ZipArchiveWriter::new(BufWriter::new(file));
    let mut compressor = zstd::bulk::Compressor::new(FRAME_ZSTD_LEVEL).map_err(io_error(path))?;
    let mut compressed = Vec::new();

    let recording = replayer.recording();
    let space = ActionSpace::new(&recording.game.buttons);
    let ram_names: Vec<String> = recording.game.ram.keys().cloned().collect();
    let mut labels = Labels::default();
    let (mut unlabelled, mut filtered, mut frame_chunks) = (0, 0, 0);
    let mut last_terminated = false;
    let mut lengths = None::<Option<u32>>;
    let mut step_frames = 0u32;
    // The observation waiting for its following frame, rendered at the end of `chunk`.
    let mut pending = None::<Observation>;
    let mut observations = 0u64;
    // The index of the last kept observation, and the current segment.
    let mut last_kept = None::<u64>;
    let mut segment = 0u32;

    let mut sprites = if options.sprites {
        let hw = recording.game.sprite_hw.ok_or_else(|| {
            mismatch(
                replayer,
                format!(
                    "game {} has no sprite labels: its game.toml (as recorded) has no \
                     sprite_hw (docs/games.md)",
                    recording.meta.game_id
                ),
            )
        })?;
        let stream = SpriteStream::bind(hw, replayer.core_mut())
            .map_err(|error| mismatch(replayer, error.to_string()))?;
        Some(Sprites::new(stream))
    } else {
        None
    };

    let mut write_chunk = |zip: &mut ZipArchiveWriter<_>, chunk: &mut Vec<u8>, index: u32| {
        compressed.clear();
        compressed.reserve(zstd::zstd_safe::compress_bound(chunk.len()));
        compressor.compress_to_buffer(chunk.as_slice(), &mut compressed)?;
        chunk.clear();
        write_member(zip, &chunk_name(index), &compressed)
    };

    loop {
        // The list the next frame draws, read before it runs (gth_sprites).
        let more = replayer.frame() < replayer.recording().inputs.len() as u64;
        if let Some(sprites) = sprites.as_mut().filter(|_| more) {
            sprites
                .read(replayer.core_mut())
                .map_err(|error| mismatch(replayer, error.to_string()))?;
        }
        let Some(input) = replayer.run_frame()? else {
            break;
        };
        if let Some(sprites) = sprites.as_mut() {
            sprites.end_frame(&mut zip).map_err(io)?;
        }
        let action = space.action_of(input.buttons);
        labels.push_frame(action);
        if let Some(observation) = pending.take() {
            let step = StepInfo {
                action,
                terminated: observation.terminated,
                episode_frame: observation.episode_frame,
            };
            let keep = filter.keep(&step);
            match action {
                None => unlabelled += 1,
                Some(_) if !keep => filtered += 1,
                Some(action) => {
                    if last_kept.is_some_and(|last| last + 1 != observation.index) {
                        segment += 1;
                    }
                    last_kept = Some(observation.index);
                    lengths = Some(match lengths {
                        None => Some(observation.frames),
                        Some(length) => length.filter(|&length| length == observation.frames),
                    });
                    last_terminated = observation.terminated;
                    labels.push(action, &observation, segment);
                }
            }
            if last_kept != Some(observation.index) {
                chunk.truncate(chunk.len() - frame_len);
            } else if chunk.len() == chunk_len {
                write_chunk(&mut zip, &mut chunk, frame_chunks).map_err(io)?;
                frame_chunks += 1;
            }
        }
        step_frames += 1;
        let end = match options.steps {
            Steps::Recorded => input.step_end,
            Steps::Every(k) => step_frames == k.get(),
        };
        if !end {
            continue;
        }
        let at = chunk.len();
        chunk.resize(at + frame_len, 0);
        replayer
            .core()
            .with_frames(|frame, previous| match previous {
                Some(previous) => pipeline.render_max(frame, previous, &mut chunk[at..]),
                None => pipeline.render(frame, &mut chunk[at..]),
            })
            .map_err(|error| mismatch(replayer, error.to_string()))?;
        let ram = replayer.read_ram_vars()?;
        pending = Some(Observation {
            index: observations,
            terminated: replayer.recording().game.terminated(&ram)?,
            ram: ram.into_values().collect(),
            episode_frame: replayer.frame(),
            frames: std::mem::take(&mut step_frames),
        });
        observations += 1;
    }
    // The last observation has no following frame, so no action: it isn't a sample.
    if pending.is_some() {
        chunk.truncate(chunk.len() - frame_len);
    }
    if !chunk.is_empty() {
        write_chunk(&mut zip, &mut chunk, frame_chunks).map_err(io)?;
        frame_chunks += 1;
    }

    let samples = labels.direction.len();
    let ram_columns = ram_names.len();
    labels.write(&mut zip, ram_columns).map_err(io)?;
    if let Some(sprites) = sprites.as_mut() {
        sprites.finish(&mut zip).map_err(io)?;
    }
    let recording = replayer.recording();
    let meta = ShardMeta {
        shard_version: SHARD_VERSION,
        source_id: recording
            .dir
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        episode_id: 0,
        game_id: recording.meta.game_id.clone(),
        fps: recording.meta.fps,
        frameskip: match options.steps {
            Steps::Recorded => lengths.flatten(),
            Steps::Every(k) => Some(k.get()),
        },
        obs_size: [options.obs_height, options.obs_width],
        crop: options
            .crop
            .map(|rect| [rect.x, rect.y, rect.width, rect.height]),
        orientation: match options.rotation {
            Rotation::None => 0,
            Rotation::Cw90 => 90,
            Rotation::Cw270 => 270,
        },
        interpolation: match options.interpolation {
            Filter::Area => "area",
            Filter::Bilinear => "bilinear",
            Filter::Bicubic => "bicubic",
        }
        .into(),
        maxpool: options.maxpool,
        grayscale: options.color == Color::Gray,
        channels: options.color.channels() as u32,
        ram_names,
        label_source: "replay".into(),
        idm_model: None,
        filter: filter.name().into(),
        samples: samples as u64,
        frames: replayer.frame(),
        chunk_samples: options.chunk_samples.get(),
        frame_chunks,
        terminated: samples > 0 && last_terminated,
        sprite_hw: sprites.as_ref().map(|s| s.stream.hw().name().into()),
        sprite_screen: sprites.as_ref().map(|s| s.stream.screen()),
        sprite_chunk_frames: sprites.as_ref().map(|_| SPRITE_CHUNK_FRAMES),
        sprite_chunks: sprites.as_ref().map(|s| s.chunks),
        recording: Some(recording.meta.clone()),
        harness: options.harness.clone(),
    };
    let json = serde_json::to_vec_pretty(&meta).expect("ShardMeta serializes");
    write_member(&mut zip, "meta.json", &json).map_err(io)?;
    let file = zip
        .finish()
        .map_err(io::Error::other)
        .and_then(|writer| writer.into_inner().map_err(io::IntoInnerError::into_error))
        .map_err(io)?;
    file.sync_all().map_err(io)?;
    let bytes = file.metadata().map_err(io)?.len();
    Ok(ShardSummary {
        path: path.to_owned(),
        samples: samples as u64,
        frames: replayer.frame(),
        unlabelled,
        filtered,
        frame_chunks,
        sprites: sprites.as_ref().map_or(0, |s| s.records),
        bytes,
    })
}

fn mismatch(replayer: &Replayer, reason: String) -> Error {
    Error::Mismatch {
        path: replayer.recording().dir.clone(),
        reason,
    }
}

/// An observation rendered into the chunk, waiting for the frame after it.
struct Observation {
    /// Its index among the replay's observations.
    index: u64,
    terminated: bool,
    ram: Vec<i64>,
    episode_frame: u64,
    /// Frames since the previous observation (or the start).
    frames: u32,
}

/// Every replayed frame's sprite list: `sprite_offsets.npy` (`uint64[frames + 1]`, frame
/// index `i`'s records are `offsets[i]..offsets[i + 1]`, as the per-frame arrays) and the
/// records in zstd chunks of [`SPRITE_CHUNK_FRAMES`] frames.
struct Sprites {
    stream: SpriteStream,
    /// The list read for the frame about to run.
    list: Vec<Sprite>,
    offsets: Vec<u64>,
    records: u64,
    /// The current chunk's records, encoded.
    chunk: Vec<u8>,
    chunk_frames: u32,
    chunks: u32,
    compressor: zstd::bulk::Compressor<'static>,
}

impl Sprites {
    fn new(stream: SpriteStream) -> Sprites {
        Sprites {
            stream,
            list: Vec::new(),
            offsets: vec![0],
            records: 0,
            chunk: Vec::new(),
            chunk_frames: 0,
            chunks: 0,
            compressor: zstd::bulk::Compressor::new(FRAME_ZSTD_LEVEL)
                .expect("zstd level 3 is valid"),
        }
    }

    fn read(&mut self, core: &mut gth_core::Core) -> gth_sprites::Result<()> {
        self.stream.read(core, &mut self.list)
    }

    /// Appends the list read before the frame that just ran.
    fn end_frame<W: Write>(&mut self, zip: &mut ZipArchiveWriter<W>) -> io::Result<()> {
        for sprite in &self.list {
            sprite.encode(&mut self.chunk);
        }
        self.records += self.list.len() as u64;
        self.offsets.push(self.records);
        self.chunk_frames += 1;
        if self.chunk_frames == SPRITE_CHUNK_FRAMES {
            self.write_chunk(zip)?;
        }
        Ok(())
    }

    fn write_chunk<W: Write>(&mut self, zip: &mut ZipArchiveWriter<W>) -> io::Result<()> {
        let compressed = self.compressor.compress(&self.chunk)?;
        write_member(zip, &sprite_chunk_name(self.chunks), &compressed)?;
        self.chunk.clear();
        self.chunk_frames = 0;
        self.chunks += 1;
        Ok(())
    }

    /// Writes the last chunk (if it holds a frame) and the offsets.
    fn finish<W: Write>(&mut self, zip: &mut ZipArchiveWriter<W>) -> io::Result<()> {
        if self.chunk_frames > 0 {
            self.write_chunk(zip)?;
        }
        let offsets: Vec<u8> = self.offsets.iter().flat_map(|v| v.to_le_bytes()).collect();
        write_member(
            zip,
            "sprite_offsets.npy",
            &npy("<u8", &[self.offsets.len()], &offsets),
        )
    }
}

/// The per-sample and per-frame arrays, written as `.npy` members after the frames.
#[derive(Default)]
struct Labels {
    direction: Vec<u8>,
    buttons: Vec<u8>,
    ram: Vec<i64>,
    episode_frame: Vec<u64>,
    segment: Vec<u32>,
    frame_direction: Vec<u8>,
    frame_buttons: Vec<u8>,
    frame_labelled: Vec<u8>,
}

impl Labels {
    fn push(&mut self, action: Action, observation: &Observation, segment: u32) {
        self.direction.push(action.direction as u8);
        self.buttons.push(action.buttons);
        self.ram.extend(&observation.ram);
        self.episode_frame.push(observation.episode_frame);
        self.segment.push(segment);
    }

    /// One replayed frame's action; a frame with no label is neutral with `frame_labelled`
    /// false.
    fn push_frame(&mut self, action: Option<Action>) {
        let labelled = action.is_some();
        let action = action.unwrap_or(Action {
            direction: Direction::Neutral,
            buttons: 0,
        });
        self.frame_direction.push(action.direction as u8);
        self.frame_buttons.push(action.buttons);
        self.frame_labelled.push(u8::from(labelled));
    }

    fn write<W: Write>(&self, zip: &mut ZipArchiveWriter<W>, ram_columns: usize) -> io::Result<()> {
        let n = self.direction.len();
        let frames = self.frame_direction.len();
        let ram: Vec<u8> = self.ram.iter().flat_map(|v| v.to_le_bytes()).collect();
        let episode_frame: Vec<u8> = self
            .episode_frame
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let segment: Vec<u8> = self.segment.iter().flat_map(|v| v.to_le_bytes()).collect();
        // A replay's labels are certain: confidence 1 for every sample.
        let confidence: Vec<u8> = (0..n).flat_map(|_| 1f32.to_le_bytes()).collect();
        write_member(zip, "direction.npy", &npy("|u1", &[n], &self.direction))?;
        write_member(zip, "buttons.npy", &npy("|u1", &[n], &self.buttons))?;
        write_member(zip, "ram.npy", &npy("<i8", &[n, ram_columns], &ram))?;
        write_member(zip, "episode_frame.npy", &npy("<u8", &[n], &episode_frame))?;
        write_member(zip, "segment.npy", &npy("<u4", &[n], &segment))?;
        write_member(zip, "confidence.npy", &npy("<f4", &[n], &confidence))?;
        write_member(
            zip,
            "frame_direction.npy",
            &npy("|u1", &[frames], &self.frame_direction),
        )?;
        write_member(
            zip,
            "frame_buttons.npy",
            &npy("|u1", &[frames], &self.frame_buttons),
        )?;
        write_member(
            zip,
            "frame_labelled.npy",
            &npy("|b1", &[frames], &self.frame_labelled),
        )
    }
}

/// An uncompressed (stored) zip member.
fn write_member<W: Write>(
    zip: &mut ZipArchiveWriter<W>,
    name: &str,
    data: &[u8],
) -> io::Result<()> {
    let (mut entry, config) = zip.new_file(name).start().map_err(io::Error::other)?;
    let mut writer = config.wrap(&mut entry);
    writer.write_all(data)?;
    let (_, output) = writer.finish().map_err(io::Error::other)?;
    entry.finish(output).map_err(io::Error::other)?;
    Ok(())
}

/// A `.npy` file (format 1.0): the header for `descr` and `shape` (C order), then `data`.
fn npy(descr: &str, shape: &[usize], data: &[u8]) -> Vec<u8> {
    let shape = match shape {
        [n] => format!("({n},)"),
        _ => format!(
            "({})",
            shape
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let mut header = format!("{{'descr': '{descr}', 'fortran_order': False, 'shape': {shape}, }}");
    // Magic (6), version (2) and length (2), then the header padded with spaces to a multiple
    // of 64 bytes, ending in a newline.
    let unpadded = 10 + header.len() + 1;
    header.extend(std::iter::repeat_n(
        ' ',
        unpadded.next_multiple_of(64) - unpadded,
    ));
    header.push('\n');
    let mut out = b"\x93NUMPY\x01\x00".to_vec();
    out.extend(
        u16::try_from(header.len())
            .expect("short header")
            .to_le_bytes(),
    );
    out.extend(header.as_bytes());
    out.extend(data);
    out
}

/// Decodes a frame chunk (a member of the shard at `path`) into `out`, which must be exactly
/// its size: `samples * height * width * channels` bytes. Fails with [`Error::Format`] if the chunk
/// is corrupt or another size.
pub fn decode_frames(data: &[u8], out: &mut [u8], path: &Path) -> Result<()> {
    decode_exact(data, out, path, "frame chunk")
}

/// Decodes a sprite chunk (a member of the shard at `path`) holding `records` records
/// ([`gth_sprites::RECORD_SIZE`] bytes each). Fails with [`Error::Format`] if the chunk is
/// corrupt or holds another number of bytes.
pub fn decode_sprites(data: &[u8], records: usize, path: &Path) -> Result<Vec<u8>> {
    let len = records
        .checked_mul(gth_sprites::RECORD_SIZE)
        .ok_or_else(|| format_error(path, format!("sprite chunk of {records} records")))?;
    let mut out = Vec::new();
    out.try_reserve_exact(len)
        .map_err(|error| format_error(path, format!("sprite chunk of {len} bytes: {error}")))?;
    out.resize(len, 0);
    decode_exact(data, &mut out, path, "sprite chunk")?;
    Ok(out)
}

fn decode_exact(data: &[u8], out: &mut [u8], path: &Path, what: &str) -> Result<()> {
    let bad = |reason: String| format_error(path, format!("{what} {reason}"));
    let mut decoder = zstd::stream::read::Decoder::with_buffer(data)
        .map_err(|error| bad(format!("can't be decoded: {error}")))?;
    decoder
        .read_exact(out)
        .map_err(|error| bad(format!("is shorter than {} bytes: {error}", out.len())))?;
    let mut extra = [0u8; 1];
    match decoder.read(&mut extra) {
        Ok(0) => Ok(()),
        Ok(_) => Err(bad(format!("is longer than {} bytes", out.len()))),
        Err(error) => Err(bad(format!("can't be decoded: {error}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npy_headers_are_padded_to_64_bytes() {
        let file = npy("<i8", &[3, 2], &[0; 48]);
        assert_eq!(&file[..8], b"\x93NUMPY\x01\x00");
        let header_len = u16::from_le_bytes([file[8], file[9]]) as usize;
        assert_eq!((10 + header_len) % 64, 0);
        let header = std::str::from_utf8(&file[10..10 + header_len]).unwrap();
        assert!(header.starts_with("{'descr': '<i8', 'fortran_order': False, 'shape': (3, 2), }"));
        assert!(header.ends_with(" \n"));
        assert_eq!(file.len(), 10 + header_len + 48);
        let header = npy("|u1", &[5], &[]);
        assert!(
            std::str::from_utf8(&header[10..])
                .unwrap()
                .contains("'shape': (5,)")
        );
    }

    #[test]
    fn decode_frames_checks_the_size() {
        let data: Vec<u8> = (0..=255).cycle().take(3000).collect();
        let chunk = zstd::bulk::compress(&data, FRAME_ZSTD_LEVEL).unwrap();
        let path = Path::new("s.npz");
        let mut out = vec![0; 3000];
        decode_frames(&chunk, &mut out, path).unwrap();
        assert_eq!(out, data);
        let error = decode_frames(&chunk, &mut [0; 2999], path).unwrap_err();
        assert!(
            error.to_string().contains("is longer than 2999 bytes"),
            "{error}"
        );
        let error = decode_frames(&chunk, &mut [0; 3001], path).unwrap_err();
        assert!(
            error.to_string().contains("is shorter than 3001 bytes"),
            "{error}"
        );
        assert!(decode_frames(b"not zstd", &mut out, path).is_err());
    }

    fn step(direction: Direction, buttons: u8, terminated: bool) -> StepInfo {
        StepInfo {
            action: Some(Action { direction, buttons }),
            terminated,
            episode_frame: 0,
        }
    }

    fn keeps(filter: &mut dyn SampleFilter, steps: &[StepInfo]) -> Vec<bool> {
        steps.iter().map(|s| filter.keep(s)).collect()
    }

    #[test]
    fn playing_keeps_the_observation_before_the_first_input_through_game_over() {
        let unlabelled = StepInfo {
            action: None,
            terminated: false,
            episode_frame: 0,
        };
        let steps = [
            step(Direction::Neutral, 0, false),
            step(Direction::Neutral, 0, true), // attract mode looking like game over
            unlabelled,                        // start pressed
            step(Direction::Neutral, 1, true), // first input, still lives == 0: kept
            step(Direction::Neutral, 0, true), // lives not set yet: not game over
            step(Direction::Neutral, 0, false), // playing
            step(Direction::Up, 0, true),      // game over: kept
            step(Direction::Up, 0, false),
        ];
        assert_eq!(
            keeps(&mut Playing::default(), &steps),
            [false, false, false, true, true, true, true, false]
        );
        assert!(keeps(&mut KeepAll, &steps).iter().all(|&k| k));
        // A start that doesn't terminate arms game over at once: dying in the next step ends
        // the episode even if terminate then holds for good.
        let quick = [
            step(Direction::Neutral, 0, true),
            step(Direction::Up, 0, false), // first input
            step(Direction::Up, 0, true),  // game over: kept
            step(Direction::Neutral, 0, true),
        ];
        assert_eq!(
            keeps(&mut Playing::default(), &quick),
            [false, true, true, false]
        );
        // A terminate on an unlabelled sample still ends the game.
        let mut ended = steps[3..6].to_vec();
        ended.push(StepInfo {
            terminated: true,
            ..unlabelled
        });
        ended.push(step(Direction::Up, 0, false));
        assert_eq!(
            keeps(&mut Playing::default(), &ended),
            [true, true, true, true, false]
        );
        assert_eq!(filter_by_name("playing").unwrap().name(), "playing");
        assert_eq!(filter_by_name("all").unwrap().name(), "all");
        assert!(filter_by_name("none").is_none());
    }
}
