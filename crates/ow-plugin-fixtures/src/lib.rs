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
//! The VST3 fixture is a third library (feature `vst3`), written directly against the `vst3` crate
//! like a third party would: a gain effect with its own controller class (parameters, state,
//! latency and an editor view) and a single-component tone instrument (notes, a level parameter).
//! Both editors draw a plain native child view, and the libraries export a few `ow_fixture_*`
//! functions the host tests call to act as the plugin's user (see [`hooks`]).

use std::path::PathBuf;

#[cfg(feature = "crash")]
mod crash;
#[cfg(feature = "gain")]
mod gain;
#[cfg(any(feature = "gain", feature = "vst3"))]
mod view;
#[cfg(feature = "vst3")]
mod vst3_gain;

/// The folder the bundles of this build profile were written to.
pub const BUNDLE_DIR: &str = env!("OW_FIXTURE_DIR");

/// File name of the CLAP bundle holding the gain effect and the tone instrument.
pub const GAIN_FILE: &str = "ow-fixture-gain.clap";
/// File name of the CLAP bundle whose plugin crashes (or hangs) when it is instantiated.
pub const CRASH_FILE: &str = "ow-fixture-crash.clap";
/// File name of the VST3 module (a bundle folder on macOS and Linux) holding the gain effect and
/// the tone instrument.
pub const VST3_FILE: &str = "ow-fixture-gain.vst3";

/// Class id of the VST3 gain effect (the `<id>` of its `vst3:<id>` kind): four 8-digit words.
pub const VST3_GAIN_ID: &str = "4F574658565354334741494E50524F43";
/// Class id of the VST3 gain effect's controller class.
pub const VST3_GAIN_CONTROLLER_ID: &str = "4F574658565354334741494E43544C52";
/// Class id of the VST3 tone instrument (one class: component and controller in one object).
pub const VST3_TONE_ID: &str = "4F57465856535433544F4E4550524F43";

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

/// Names of the functions every fixture library exports for the host tests, which stand in for
/// what a plugin's user does in its editor (they run in the library's own process, on whichever
/// thread calls them; VST3 and CLAP editors are only ever touched from the plugin main thread, so
/// the tests call them there):
///
/// * `ow_fixture_simulate_edit(param: u32, value: f64, phase: u32)`: the user's gesture on a
///   parameter in the open editor. `phase` 0 takes hold of the control, 1 changes it to the plain
///   `value`, 2 lets go.
/// * `ow_fixture_request_resize(width: u32, height: u32) -> u32`: the editor asks its host for a
///   new size; 1 when the host accepted.
/// * `ow_fixture_editor_state(out: *mut EditorState)`: whether the editor's view is attached and
///   how big it is.
pub mod hooks {
    /// Name of the simulated-gesture function.
    pub const SIMULATE_EDIT: &[u8] = b"ow_fixture_simulate_edit\0";
    /// Name of the resize-request function.
    pub const REQUEST_RESIZE: &[u8] = b"ow_fixture_request_resize\0";
    /// Name of the editor-state function.
    pub const EDITOR_STATE: &[u8] = b"ow_fixture_editor_state\0";
    /// Gesture phase: take hold of the control.
    pub const PHASE_BEGIN: u32 = 0;
    /// Gesture phase: change the value.
    pub const PHASE_CHANGE: u32 = 1;
    /// Gesture phase: let go.
    pub const PHASE_END: u32 = 2;

    /// What `ow_fixture_editor_state` fills in.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct EditorState {
        /// 1 while the editor's view is attached to a host window.
        pub attached: u32,
        /// Width of the view in pixels.
        pub width: u32,
        /// Height of the view in pixels.
        pub height: u32,
    }
}

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

/// Path of the VST3 module (the file, or the `.vst3` folder on macOS and Linux).
pub fn vst3_path() -> PathBuf {
    bundle_dir().join(VST3_FILE)
}

/// The shared library inside a fixture module, for tests that call its exported hooks: the file
/// itself for a plain library, the binary inside the bundle otherwise.
pub fn library_of(module: &std::path::Path) -> PathBuf {
    if module.is_file() {
        return module.to_path_buf();
    }
    let folder = if cfg!(target_os = "macos") {
        "MacOS"
    } else if cfg!(windows) {
        "x86_64-win"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64-linux"
    } else {
        "x86_64-linux"
    };
    let dir = module.join("Contents").join(folder);
    std::fs::read_dir(&dir)
        .ok()
        .and_then(|entries| {
            let mut files: Vec<PathBuf> = entries
                .filter_map(|entry| Some(entry.ok()?.path()))
                .filter(|path| path.is_file())
                .collect();
            files.sort();
            files.into_iter().next()
        })
        .unwrap_or_else(|| dir.join("missing"))
}
