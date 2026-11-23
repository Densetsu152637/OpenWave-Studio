//! Fixture plugins for OpenWave Studio's plugin-hosting tests.
//!
//! This crate is built twice by its own `build.rs`, as two shared libraries (see there): the
//! `gain` fixture (a gain effect that reports a fixed latency, plus a tiny instrument) and the
//! `crash` fixture, which aborts or hangs as soon as a host instantiates it, to prove that the
//! scanner survives a bad plugin. The results are written as plugin bundles to
//! `target/plugin-fixtures/` (release builds) or `target/plugin-fixtures-debug/` (any other
//! profile); `cargo build -p ow-plugin-fixtures --release` is the documented way to produce them.
//!
//! The crate itself (what other crates depend on) is a plain library of constants and paths: it
//! tells tests where the bundles are and what is in them. Depending on it (for example as a
//! dev-dependency) also builds the bundles, so a fresh checkout needs no extra step.
//!
//! VST3 fixtures will be added to the same crate (and the same `build.rs`) by the next slice.

use std::path::PathBuf;

#[cfg(feature = "crash")]
mod crash;
#[cfg(feature = "gain")]
mod gain;

/// The folder the bundles of this build profile were written to.
pub const BUNDLE_DIR: &str = env!("OW_FIXTURE_DIR");

/// File name of the CLAP bundle holding the gain effect and the tone instrument.
pub const GAIN_FILE: &str = "ow-fixture-gain.clap";
/// File name of the CLAP bundle whose plugin crashes (or hangs) when it is instantiated.
pub const CRASH_FILE: &str = "ow-fixture-crash.clap";

/// Plugin id of the gain effect.
pub const GAIN_ID: &str = "com.openwave.fixture.gain";
/// Plugin id of the tone instrument.
pub const TONE_ID: &str = "com.openwave.fixture.tone";
/// Plugin id of the crashing plugin.
pub const CRASH_ID: &str = "com.openwave.fixture.crash";

/// Latency the gain effect reports and applies, in frames.
pub const GAIN_LATENCY: u32 = 64;
/// Parameter id of the gain effect's gain (0 to 2, default 1).
pub const PARAM_GAIN: u32 = 1;
/// Parameter id of the gain effect's polarity (an enumeration: 0 normal, 1 inverted).
pub const PARAM_POLARITY: u32 = 2;
/// Parameter id of the tone instrument's level (0 to 1, default 0.5).
pub const PARAM_LEVEL: u32 = 1;

/// Environment variable read by the crash fixture: `hang` makes it sleep forever instead of
/// aborting.
pub const CRASH_MODE_ENV: &str = "OW_FIXTURE_CRASH_MODE";

/// Where the bundles live.
pub fn bundle_dir() -> PathBuf {
    PathBuf::from(BUNDLE_DIR)
}

/// Path of the gain and tone bundle (the file, or the `.clap` folder on macOS).
pub fn gain_path() -> PathBuf {
    bundle_dir().join(GAIN_FILE)
}

/// Path of the crashing bundle.
pub fn crash_path() -> PathBuf {
    bundle_dir().join(CRASH_FILE)
}
