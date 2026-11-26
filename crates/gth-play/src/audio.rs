//! Core audio to the output device's rate (docs/human-play.md, "Audio"). Frames are paced
//! by a timer at the core's frame rate, not by the audio device, so the two clocks drift:
//! [`rate_step`] nudges the resampling ratio by up to [`MAX_ADJUST`] to keep the device's
//! queue near its target (dynamic rate control), which no ear notices.

use std::collections::VecDeque;

/// The largest change to the resampling ratio (0.5%).
pub const MAX_ADJUST: f64 = 0.005;

/// Input frames per output frame for a queue holding `queued` output frames, aiming at
/// `target`: the nominal `core_rate / device_rate`, sped up (consuming input faster,
/// making fewer output frames) when the queue is above target and slowed when below.
pub fn rate_step(core_rate: f64, device_rate: f64, queued: usize, target: usize) -> f64 {
    let target = target.max(1) as f64;
    let error = ((queued as f64 - target) / target).clamp(-1.0, 1.0);
    core_rate / device_rate * (1.0 + MAX_ADJUST * error)
}

/// A linear-interpolating stereo resampler that carries its position across calls, so a
/// stream resampled in pieces matches it resampled at once.
#[derive(Debug, Clone, Default)]
pub struct Resampler {
    /// The next output frame's position past `prev`, in input frames.
    pos: f64,
    /// The last input frame of the previous call.
    prev: [f32; 2],
}

impl Resampler {
    /// Resamples `input` (interleaved stereo `i16`) at `step` input frames per output frame,
    /// appending interleaved stereo `f32` in [-1, 1] to `out`.
    pub fn process(&mut self, input: &[i16], step: f64, out: &mut VecDeque<f32>) {
        let n = input.len() / 2;
        if n == 0 {
            return;
        }
        let frame = |i: usize| -> [f32; 2] {
            [
                f32::from(input[2 * i]) / 32768.0,
                f32::from(input[2 * i + 1]) / 32768.0,
            ]
        };
        // Position p is between frame i-1 and frame i for i = floor(p), frame -1 being prev.
        while self.pos < n as f64 {
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;
            let a = if i == 0 { self.prev } else { frame(i - 1) };
            let b = frame(i);
            out.push_back(a[0] + (b[0] - a[0]) * t);
            out.push_back(a[1] + (b[1] - a[1]) * t);
            self.pos += step;
        }
        self.pos -= n as f64;
        self.prev = frame(n - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_track_the_queue() {
        assert_eq!(rate_step(48_000.0, 48_000.0, 100, 100), 1.0);
        assert_eq!(rate_step(48_000.0, 48_000.0, 1000, 100), 1.005);
        assert_eq!(rate_step(48_000.0, 48_000.0, 0, 100), 0.995);
        assert!((rate_step(32_000.0, 48_000.0, 100, 100) - 2.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn same_rate_reproduces_the_input_one_frame_late() {
        let input: Vec<i16> = (0..8).map(|i| i * 1000).collect();
        let mut out = VecDeque::new();
        Resampler::default().process(&input, 1.0, &mut out);
        let expected: Vec<f32> = [0, 0]
            .into_iter()
            .chain(input[..6].iter().copied())
            .map(|s| f32::from(s) / 32768.0)
            .collect();
        assert_eq!(out.into_iter().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn pieces_match_one_call() {
        let input: Vec<i16> = (0..800).map(|i| ((i * 37) % 2000 - 1000) as i16).collect();
        let step = 0.5; // exact in binary, so both pass through the same positions
        let mut whole = VecDeque::new();
        Resampler::default().process(&input, step, &mut whole);
        let mut pieces = VecDeque::new();
        let mut resampler = Resampler::default();
        for chunk in input.chunks(2 * 133) {
            resampler.process(chunk, step, &mut pieces);
        }
        assert_eq!(whole.len(), pieces.len());
        for (a, b) in whole.iter().zip(&pieces) {
            assert!((a - b).abs() < 1e-6);
        }
        assert_eq!(whole.len(), 2 * 800, "400 frames at half the step");
    }
}
