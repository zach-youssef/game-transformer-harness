//! The core's audio on the default output device, through cpal (docs/human-play.md, "Audio").

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use gth_play::audio::{Resampler, rate_step};

/// How much audio the device's queue aims to hold: enough to ride out a late frame, little
/// enough that sound stays in step with the picture.
const TARGET_SECONDS: f64 = 0.05;

pub struct Sound {
    /// Started by the first samples, so it doesn't underrun while the window opens.
    stream: Stream,
    started: bool,
    queue: Arc<Mutex<VecDeque<f32>>>,
    /// Output frames the device wanted and the queue didn't have.
    underruns: Arc<AtomicU64>,
    resampler: Resampler,
    core_rate: f64,
    device_rate: f64,
}

impl Sound {
    /// Opens the default output device; `None` (having said why) if there is none or it
    /// fails, so the game still plays, silently.
    pub fn new(core_rate: f64) -> Option<Sound> {
        match Sound::open(core_rate) {
            Ok(sound) => Some(sound),
            Err(error) => {
                eprintln!("gth-play: no audio ({error})");
                None
            }
        }
    }

    fn open(core_rate: f64) -> Result<Sound, String> {
        if !core_rate.is_finite() || core_rate <= 0.0 {
            return Err(format!("the core's sample rate is {core_rate}"));
        }
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no output device")?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let config: StreamConfig = supported.config();
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let underruns = Arc::new(AtomicU64::new(0));
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config, &queue, &underruns),
            SampleFormat::I16 => build::<i16>(&device, &config, &queue, &underruns),
            SampleFormat::U16 => build::<u16>(&device, &config, &queue, &underruns),
            SampleFormat::I32 => build::<i32>(&device, &config, &queue, &underruns),
            other => return Err(format!("unsupported sample format {other}")),
        }?;
        Ok(Sound {
            stream,
            started: false,
            queue,
            underruns,
            resampler: Resampler::default(),
            core_rate,
            device_rate: f64::from(config.sample_rate),
        })
    }

    /// Queues one frame's samples (interleaved stereo at the core's rate).
    pub fn push(&mut self, samples: &[i16]) {
        let target = (self.device_rate * TARGET_SECONDS) as usize;
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let queued = queue.len() / 2;
        // Far behind the device (after a stall): drop the excess rather than lag.
        if queued > 4 * target {
            let keep = 2 * target;
            let excess = queue.len() - keep;
            queue.drain(..excess);
        }
        let step = rate_step(self.core_rate, self.device_rate, queue.len() / 2, target);
        self.resampler.process(samples, step, &mut queue);
        drop(queue);
        if !self.started {
            self.started = true;
            self.set_paused(false);
        }
    }

    /// Stops or restarts the device (pausing the game would otherwise underrun).
    pub fn set_paused(&mut self, paused: bool) {
        if !self.started {
            return;
        }
        let result = if paused {
            self.stream.pause()
        } else {
            self.stream.play()
        };
        if let Err(error) = result {
            eprintln!("gth-play: audio: {error}");
        }
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &StreamConfig,
    queue: &Arc<Mutex<VecDeque<f32>>>,
    underruns: &Arc<AtomicU64>,
) -> Result<Stream, String> {
    let channels = usize::from(config.channels);
    let (queue, underruns) = (queue.clone(), underruns.clone());
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _| {
                let mut queue = queue.lock().unwrap_or_else(PoisonError::into_inner);
                let mut missing = 0;
                for frame in data.chunks_mut(channels) {
                    let (l, r) = match (queue.pop_front(), queue.pop_front()) {
                        (Some(l), Some(r)) => (l, r),
                        _ => {
                            missing += 1;
                            (0.0, 0.0)
                        }
                    };
                    for (c, out) in frame.iter_mut().enumerate() {
                        let value = match (channels, c) {
                            (1, _) => (l + r) / 2.0,
                            (_, 0) => l,
                            (_, 1) => r,
                            _ => 0.0,
                        };
                        *out = T::from_sample(value);
                    }
                }
                if missing > 0 {
                    underruns.fetch_add(missing, Ordering::Relaxed);
                }
            },
            |error| eprintln!("gth-play: audio: {error}"),
            None,
        )
        .map_err(|e| e.to_string())
}
