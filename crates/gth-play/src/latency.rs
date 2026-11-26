//! The game's share of input latency: how many frames after a press the core's frame first
//! shows it (docs/human-play.md, "Input latency"). The player measures the rest, from the
//! input event to the frame's present, while it runs.

use gth_core::Core;

/// Runs `max_frames` frames from `state` twice, once with nothing pressed and once with
/// `buttons` held from the first frame, and returns the first frame (1-based) whose picture
/// differs: 1 means the frame run with the press already shows it. `None` if none differs
/// within `max_frames`. An error if the core doesn't run the same from the same state (a
/// difference then wouldn't be the press's): the no-press run is done twice and compared.
pub fn frames_to_response(
    core: &mut Core,
    state: &[u8],
    buttons: u16,
    max_frames: u32,
) -> Result<Option<u32>, String> {
    let mut run = |buttons: u16| -> Result<Vec<Vec<u8>>, String> {
        core.unserialize(state).map_err(|e| e.to_string())?;
        Ok((0..max_frames)
            .map(|_| {
                core.run_frames(1, buttons);
                core.with_frame(|frame| {
                    let mut bytes = frame.data.to_vec();
                    bytes.extend_from_slice(&(frame.width as u64).to_le_bytes());
                    bytes.extend_from_slice(&(frame.height as u64).to_le_bytes());
                    bytes
                })
            })
            .collect())
    };
    let idle = run(0)?;
    if run(0)? != idle {
        return Err("the core doesn't run the same twice from the state".into());
    }
    let pressed = run(buttons)?;
    core.unserialize(state).map_err(|e| e.to_string())?;
    Ok(idle
        .iter()
        .zip(&pressed)
        .position(|(a, b)| a != b)
        .map(|i| i as u32 + 1))
}

/// Percentiles of event-to-present times, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub samples: usize,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

/// Summarizes `ms` (sorted in place); `None` if empty.
pub fn summarize(ms: &mut [f64]) -> Option<Summary> {
    if ms.is_empty() {
        return None;
    }
    ms.sort_by(f64::total_cmp);
    let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
    Some(Summary {
        samples: ms.len(),
        p50: at(0.5),
        p95: at(0.95),
        max: ms[ms.len() - 1],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries() {
        assert_eq!(summarize(&mut []), None);
        let mut ms: Vec<f64> = (1..=100).rev().map(f64::from).collect();
        let s = summarize(&mut ms).unwrap();
        assert_eq!((s.samples, s.p50, s.p95, s.max), (100, 51.0, 95.0, 100.0));
    }
}
