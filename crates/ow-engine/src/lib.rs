//! Real-time audio engine for OpenWave Studio.
//!
//! Stage 1 is the smoke path: an [`Engine`] handle that opens the default output and plays a
//! 440 Hz test tone on request. Later stages grow the graph behind the same handle; the audio
//! callback must stay allocation-free and lock-free, so control flows in through atomics (and,
//! from stage 5, wait-free rings).

use std::f32::consts::TAU;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use ow_io::{OutputInfo, OutputStream};

/// Test tone frequency in hertz.
pub const TEST_TONE_HZ: f32 = 440.0;
/// Test tone peak amplitude (linear), about -14 dBFS so it is safe on loud outputs.
const TEST_TONE_GAIN: f32 = 0.2;
/// Fade time in seconds when the tone starts or stops, to avoid clicks.
const FADE_SECONDS: f32 = 0.01;
/// Weight of the newest block in the smoothed CPU load.
const CPU_SMOOTHING: f32 = 0.1;

/// Why the engine could not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError(String);

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EngineError {}

/// Sine generator with a linear fade, so toggling it never clicks.
#[derive(Debug, Clone)]
pub struct ToneGenerator {
    phase: f32,
    phase_step: f32,
    level: f32,
    level_step: f32,
}

impl ToneGenerator {
    pub fn new(sample_rate: u32, frequency: f32) -> Self {
        let rate = sample_rate.max(1) as f32;
        Self {
            phase: 0.0,
            phase_step: TAU * frequency / rate,
            level: 0.0,
            level_step: TEST_TONE_GAIN / (FADE_SECONDS * rate),
        }
    }

    /// Adds the tone to an interleaved buffer (the same value on every channel of a frame).
    /// `active` selects whether the level fades towards full gain or towards silence.
    pub fn render(&mut self, buf: &mut [f32], channels: usize, active: bool) {
        let channels = channels.max(1);
        for frame in buf.chunks_mut(channels) {
            self.level = if active {
                (self.level + self.level_step).min(TEST_TONE_GAIN)
            } else {
                (self.level - self.level_step).max(0.0)
            };
            let value = self.phase.sin() * self.level;
            self.phase += self.phase_step;
            if self.phase >= TAU {
                self.phase -= TAU;
            }
            for sample in frame {
                *sample += value;
            }
        }
    }
}

#[derive(Debug, Default)]
struct Shared {
    tone: AtomicBool,
    /// Smoothed callback load (0.0 to 1.0 and above when overrunning), as `f32` bits.
    cpu: AtomicU32,
}

/// Handle to the running engine. `Send + Sync`; dropping it stops audio.
pub struct Engine {
    shared: Arc<Shared>,
    stream: OutputStream,
}

impl Engine {
    /// Opens the default output device and starts the (silent) engine.
    pub fn start() -> Result<Self, EngineError> {
        let shared = Arc::new(Shared::default());
        let callback_shared = Arc::clone(&shared);
        let stream = OutputStream::open_default(move |info: &OutputInfo| {
            let mut tone = ToneGenerator::new(info.sample_rate, TEST_TONE_HZ);
            let sample_rate = info.sample_rate.max(1) as f32;
            let mut cpu = 0.0_f32;
            move |buf: &mut [f32], channels: usize| {
                let started = Instant::now();
                tone.render(buf, channels, callback_shared.tone.load(Ordering::Relaxed));
                let frames = (buf.len() / channels.max(1)) as f32;
                let budget = frames / sample_rate;
                if budget > 0.0 {
                    let load = started.elapsed().as_secs_f32() / budget;
                    cpu += (load - cpu) * CPU_SMOOTHING;
                    callback_shared.cpu.store(cpu.to_bits(), Ordering::Relaxed);
                }
            }
        })
        .map_err(|error| EngineError(error.to_string()))?;
        Ok(Self { shared, stream })
    }

    /// The opened output device and format.
    pub fn output(&self) -> &OutputInfo {
        self.stream.info()
    }

    /// Starts or stops the 440 Hz test tone.
    pub fn set_test_tone(&self, on: bool) {
        self.shared.tone.store(on, Ordering::Relaxed);
    }

    pub fn test_tone(&self) -> bool {
        self.shared.tone.load(Ordering::Relaxed)
    }

    /// Smoothed audio callback load, 0.0 (idle) to 1.0 (the whole buffer period).
    pub fn cpu_load(&self) -> f32 {
        f32::from_bits(self.shared.cpu.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_block(
        tone: &mut ToneGenerator,
        frames: usize,
        channels: usize,
        active: bool,
    ) -> Vec<f32> {
        let mut buf = vec![0.0; frames * channels];
        tone.render(&mut buf, channels, active);
        buf
    }

    #[test]
    fn inactive_tone_is_silent() {
        let mut tone = ToneGenerator::new(48_000, TEST_TONE_HZ);
        assert!(
            render_block(&mut tone, 4_800, 2, false)
                .iter()
                .all(|s| *s == 0.0)
        );
    }

    #[test]
    fn active_tone_has_the_requested_frequency() {
        let rate = 48_000;
        let mut tone = ToneGenerator::new(rate, TEST_TONE_HZ);
        // Skip the fade-in, then count upward zero crossings over exactly one second.
        render_block(&mut tone, 4_800, 1, true);
        let buf = render_block(&mut tone, rate as usize, 1, true);
        let crossings = buf.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((439..=441).contains(&crossings), "crossings = {crossings}");
        let peak = buf.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((peak - TEST_TONE_GAIN).abs() < 0.01, "peak = {peak}");
    }

    #[test]
    fn tone_is_copied_to_every_channel_and_added_to_existing_audio() {
        let mut tone = ToneGenerator::new(48_000, TEST_TONE_HZ);
        render_block(&mut tone, 4_800, 2, true);
        let mut buf = vec![0.5; 2 * 100];
        tone.render(&mut buf, 2, true);
        assert!(buf.chunks(2).all(|f| f[0] == f[1]));
        assert!(buf.iter().any(|s| *s != 0.5));
    }

    #[test]
    fn tone_fades_out_without_a_jump() {
        let mut tone = ToneGenerator::new(48_000, TEST_TONE_HZ);
        render_block(&mut tone, 4_800, 1, true);
        let buf = render_block(&mut tone, 4_800, 1, false);
        let max_step = buf
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_step < 0.05, "max step = {max_step}");
        assert!(buf[buf.len() - 1].abs() < 1e-6);
    }
}
