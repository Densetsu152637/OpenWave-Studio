//! The UI bridge thread: samples the [`TelemetryBank`] at up to 60 Hz and hands each packed
//! frame (little-endian `f32` words, layout in `ow_ipc::telemetry`) to a sender, which in the app
//! is a Tauri `ipc::Channel` carrying raw bytes. It is the only thread that reads the bank and
//! it never touches the audio thread: sampling is relaxed atomic loads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ow_ipc::{FramePacker, Subscriptions, TelemetryBank};

/// Highest sampling rate in hertz.
pub const MAX_HZ: u32 = 60;

/// A running bridge thread; dropping it stops and joins the thread.
pub struct Bridge {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Bridge {
    /// Starts sampling `bank` at `hz` (clamped to 1..=60). `before_sample` runs on the bridge
    /// thread right before each frame is packed (the app uses it to copy counters that live
    /// outside the bank); `send` receives each frame and returns false when the receiver is
    /// gone, which ends the thread.
    pub fn start(
        bank: Arc<TelemetryBank>,
        subscriptions: Arc<Subscriptions>,
        hz: u32,
        mut before_sample: impl FnMut(&TelemetryBank) + Send + 'static,
        mut send: impl FnMut(&[u8]) -> bool + Send + 'static,
    ) -> Self {
        let period = Duration::from_micros(1_000_000 / u64::from(hz.clamp(1, MAX_HZ)));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("ow-ui-bridge".into())
            .spawn(move || {
                let mut packer = FramePacker::new();
                let mut next = Instant::now();
                while !flag.load(Ordering::Relaxed) {
                    before_sample(&bank);
                    if !send(packer.pack(&bank, &subscriptions)) {
                        break;
                    }
                    next += period;
                    let now = Instant::now();
                    if next > now {
                        thread::sleep(next - now);
                    } else {
                        // Fell behind (a stalled receiver): do not burst to catch up.
                        next = now;
                    }
                }
            })
            .ok();
        Self { stop, handle }
    }

    /// True while the thread runs.
    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Stops the thread and waits for it.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ow_ipc::Frame;

    use super::*;

    fn run_for(ms: u64, hz: u32, subscribed: &[u16]) -> Vec<Frame> {
        let bank = Arc::new(TelemetryBank::new());
        bank.write_meter(2, [0.5, 0.25], [0.1, 0.1]);
        bank.set_position(48_000);
        let subscriptions = Arc::new(Subscriptions::new());
        subscriptions.set(subscribed);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&frames);
        let mut bridge = Bridge::start(
            bank,
            subscriptions,
            hz,
            |bank| bank.set_overflow(ow_ipc::Overflow::Midi, 7),
            move |bytes| {
                sink.lock()
                    .unwrap()
                    .push(Frame::decode(bytes).expect("valid frame"));
                true
            },
        );
        thread::sleep(Duration::from_millis(ms));
        bridge.stop();
        assert!(!bridge.is_running());
        std::mem::take(&mut *frames.lock().unwrap())
    }

    #[test]
    fn frames_arrive_at_the_requested_rate_and_carry_the_bank() {
        let frames = run_for(350, 30, &[2, 3]);
        // 30 Hz for 0.35 s is about 11 frames; allow scheduler slack either way but never more
        // than the rate allows.
        assert!((4..=13).contains(&frames.len()), "{} frames", frames.len());
        let first = &frames[0];
        assert_eq!(first.position, 48_000);
        assert_eq!(first.midi_dropped, 7);
        assert_eq!(
            first.strips.iter().map(|s| s.slot).collect::<Vec<_>>(),
            [2, 3],
            "only subscribed strips"
        );
        assert_eq!(first.strips[0].peak, [0.5, 0.25]);
        // The peak was consumed by the first frame.
        assert_eq!(frames[1].strips[0].peak, [0.0, 0.0]);
        let counters: Vec<u32> = frames.iter().map(|f| f.counter).collect();
        assert!(counters.windows(2).all(|w| w[1] == w[0] + 1));
    }

    #[test]
    fn the_rate_is_capped_at_60_hz() {
        let frames = run_for(250, 10_000, &[]);
        assert!(frames.len() <= 17, "{} frames in 250 ms", frames.len());
        assert!(frames.iter().all(|f| f.strips.is_empty()));
    }

    #[test]
    fn a_gone_receiver_ends_the_thread() {
        let bank = Arc::new(TelemetryBank::new());
        let bridge = Bridge::start(bank, Arc::new(Subscriptions::new()), 60, |_| {}, |_| false);
        let deadline = Instant::now() + Duration::from_secs(5);
        while bridge.is_running() {
            assert!(Instant::now() < deadline, "thread did not end");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
