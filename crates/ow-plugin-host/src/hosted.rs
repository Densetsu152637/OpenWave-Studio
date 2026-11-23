//! [`HostedPlugin`]: one plugin instance, whatever its format, as the rest of the app sees it.
//!
//! The split follows the threads. A [`HostedPlugin`] is the control-side object: it is owned by
//! the [`PluginCache`](crate::PluginCache), answers main-thread questions (state, parameter
//! values) through the [`MainThreadExecutor`](crate::MainThreadExecutor) and queues parameter
//! edits. Its [`PluginProcessor`] is the audio-thread side: the engine's device node owns one
//! and calls [`process`](PluginProcessor::process) once per block. Both sides share the same
//! plugin instance, which is why a recompile (a new node) does not re-instantiate anything.
//!
//! CLAP implements both in [`crate::clap`] and VST3 in [`crate::vst3`].

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use ow_dsp::Event;

use crate::executor::ExecutorError;
use crate::info::PluginInfo;

/// Why a plugin operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    /// The main thread could not run the call.
    Executor(ExecutorError),
    /// The plugin or the format layer refused or failed, with a reason.
    Plugin(String),
    /// The plugin was already released.
    Destroyed,
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Executor(error) => error.fmt(f),
            Self::Plugin(message) => f.write_str(message),
            Self::Destroyed => f.write_str("the plugin instance was released"),
        }
    }
}

impl std::error::Error for PluginError {}

impl From<ExecutorError> for PluginError {
    fn from(error: ExecutorError) -> Self {
        Self::Executor(error)
    }
}

/// Where a parameter change made in the plugin's own editor is in its gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditPhase {
    /// The user took hold of the control (VST3 `beginEdit`, CLAP gesture begin).
    Begin,
    /// The value changed.
    Change,
    /// The user let go.
    End,
}

/// A parameter change made in the plugin's own editor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EditorEdit {
    /// The plugin's parameter id.
    pub param: u32,
    /// Where it is in the gesture.
    pub phase: EditPhase,
    /// The parameter's plain value (the current one for [`EditPhase::Begin`] and
    /// [`EditPhase::End`]).
    pub value: f64,
}

/// Receives the parameter changes made in a plugin's editor. Called on whatever thread the
/// plugin reports them (the plugin main thread, or the thread that calls
/// [`HostedPlugin::service`]), so it must not block on anything the main thread holds: the app's
/// sink hands them to another thread.
pub type EditSink = Arc<dyn Fn(EditorEdit) + Send + Sync>;

/// Things a plugin asked the host for since the last [`HostedPlugin::service`] call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Requests {
    /// The plugin's latency changed (already re-read); the graph must be recompiled for delay
    /// compensation to follow.
    pub latency_changed: bool,
    /// The plugin's state changed on its own (an editor gesture, a preset load).
    pub state_dirty: bool,
    /// The plugin's parameter list or values changed on its own.
    pub params_rescanned: bool,
}

impl Requests {
    /// True when anything was requested.
    pub fn any(&self) -> bool {
        self.latency_changed || self.state_dirty || self.params_rescanned
    }
}

/// The audio-thread half of a hosted plugin. Owned by one device node at a time.
pub trait PluginProcessor: Send {
    /// Renders one block in place: `audio` holds the plugin's input (ignored by instruments) on
    /// entry and its output on return. Both slices have the same length. Realtime: no allocation
    /// of this crate's own, no blocking; the plugin's internals are the plugin's business.
    fn process(&mut self, audio: &mut [&mut [f32]; 2], events: &[Event]);

    /// Forgets sounding notes and tails.
    fn reset(&mut self);

    /// Frames by which the plugin delays its output, as last reported.
    fn latency_frames(&self) -> u32;
}

/// One plugin instance.
pub trait HostedPlugin: Send + Sync {
    /// What the scanner learned about the plugin.
    fn info(&self) -> &PluginInfo;

    /// A processor for a device node, sharing this instance. Any number may exist over time (one
    /// per compile) but only the node in the live plan processes.
    fn processor(&self) -> Box<dyn PluginProcessor>;

    /// The latency the plugin currently reports, in frames.
    fn latency_frames(&self) -> u32;

    /// Brings the instance's parameters in line with `params` (the model's values by plugin
    /// parameter id): every parameter that differs from what was last sent is queued as a
    /// parameter event for the next block, with no re-instantiation. A parameter that was set
    /// before and is now missing returns to its default. Returns how many events were queued.
    /// A CLAP instance takes no main-thread call here (the edits go through a lock-free queue); a
    /// VST3 instance converts plain values to normalised ones through its controller, which is a
    /// main-thread call.
    fn set_params(&self, params: &BTreeMap<u32, f64>) -> usize;

    /// The plugin's own current value of parameter `id` (a main-thread call).
    fn param_value(&self, id: u32) -> Result<Option<f64>, PluginError>;

    /// Serialises the plugin's state (a main-thread call).
    fn save_state(&self) -> Result<Vec<u8>, PluginError>;

    /// Restores a state written by [`save_state`](Self::save_state) (a main-thread call).
    fn load_state(&self, state: &[u8]) -> Result<(), PluginError>;

    /// Serves what the plugin asked of the host (main-thread callbacks, latency re-reads) and
    /// reports it.
    fn service(&self) -> Requests;

    /// Makes the instance ready for `sample_rate` and blocks of up to `max_block` frames
    /// (re-activating it when they changed). Cheap when nothing changed.
    fn configure(&self, sample_rate: u32, max_block: usize) -> Result<(), PluginError>;

    /// Sets where parameter changes made in the plugin's editor go (replacing an earlier sink).
    /// The values arrive as plain values, the same as `set_params` takes. Changes the host
    /// itself makes through [`set_params`](Self::set_params) are never reported back.
    fn set_edit_sink(&self, sink: EditSink);

    /// Opens the plugin's editor in a window OpenWave creates, or brings it forward when it is
    /// open (a main-thread call). Fails when the plugin has no editor or the platform cannot
    /// show one ([`crate::editor::AVAILABLE`]).
    fn open_editor(&self) -> Result<(), PluginError>;

    /// Closes the editor window and releases the plugin's editor (a main-thread call).
    /// Idempotent; also what removing the device or closing the project does.
    fn close_editor(&self);

    /// True while the editor window is open.
    fn editor_open(&self) -> bool;

    /// The editor window's client size in pixels, when it is open (a main-thread call).
    fn editor_size(&self) -> Option<(u32, u32)>;

    /// Deactivates and destroys the instance. Idempotent. Processors that outlive it output
    /// silence (effects pass their input through).
    fn destroy(&self);
}
