//! Tauri side of the IPC bridge (architecture section 7).
//!
//! All commands except the strip subscription run on the blocking pool (`blocking`), so the
//! control lock, plan compiles and thread joins never stall the async runtime.
//!
//! * `ipc_attach` / `ipc_snapshot` / `ipc_command` / `ipc_alloc_ids`: the state mirror. The
//!   project lives in [`control::Control`] behind one mutex on the control side; every command
//!   runs there, its patch goes out on an `ipc::Channel` while the lock is held (that is what
//!   keeps patches ordered), and the edited project is compiled and swapped into the engine
//!   ([`ow_engine::Engine::set_project`]). The audio callback never sees the project.
//! * `telemetry_start` / `telemetry_set_strips` / `telemetry_stop`: the 60 Hz bridge thread
//!   ([`bridge::Bridge`]) that sends packed `f32` frames over a second channel as raw bytes.
//! * `peaks_range`: waveform min/max pairs as a raw-byte `ipc::Response` ([`peaks::PeakStore`],
//!   served from the `ow-media` peak cache).
//! * `project_new` / `project_open` / `project_save` ([`project`]), `media_import` ([`media`])
//!   and `export_wav` ([`export`]): project files, importing audio and the offline mixdown. They
//!   are plain methods on [`IpcState`] (testable without a window); the commands below only
//!   hand them the engine as the [`control::PlanTarget`].
//!
//! * `export_mix` ([`export`]) and `analyze` ([`analysis`]): export with optional loudness
//!   normalisation, and offline measurement of the master or one strip. Plain `IpcState`
//!   methods like the rest.
//! * `device_descriptors` (in this module): the built-in devices' parameter descriptors from
//!   `ow-dsp`, for the generic device panel. MIDI tracks and their synth are plain model edits
//!   (`insert_track` with an instrument device, `add_clip` with MIDI notes, `set_device_param`),
//!   so they need no command of their own; the engine plays them after each recompile.
//! * `record_set_arm` / `record_start` ([`record`]): arming audio tracks (session state shown in
//!   the mirror) and recording on them; `transport_stop` ends a recording and adds the takes.
//!
//! What is not wired yet: fine-grained updates that skip the recompile (fader and mute changes
//! could travel as `Message::SetParam` using `ow_engine::fader_gain`) and telemetry for strips
//! beyond the 256 meter slots.

pub mod analysis;
pub mod bridge;
pub mod control;
pub mod export;
pub mod media;
pub mod peaks;
pub mod project;
pub mod record;

#[cfg(test)]
mod mix;
#[cfg(test)]
mod mvp;
#[cfg(test)]
mod mvp_record;
#[cfg(test)]
mod plugin_state;

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use ow_ipc::{
    CommandReply, MediaImportReply, ModelPatch, Overflow, Snapshot, Subscriptions, UiCommand,
};
use tauri::ipc::{Channel, InvokeResponseBody, Response};
use tauri::{AppHandle, Manager, State};

use crate::engine::EngineState;
use control::{Control, PatchSink};

impl PatchSink for Channel<ModelPatch> {
    fn send(&self, patch: &ModelPatch) -> bool {
        Channel::send(self, patch.clone()).is_ok()
    }
}

/// Managed state of the IPC bridge.
pub struct IpcState {
    control: Mutex<Control>,
    subscriptions: Arc<Subscriptions>,
    bridge: Mutex<Option<bridge::Bridge>>,
    peaks: peaks::PeakStore,
    /// Peak files being built in the background after an open or save.
    builds: Mutex<Vec<JoinHandle<std::io::Result<()>>>>,
    /// The recording in progress, if any (see [`record`]).
    recording: Mutex<Option<record::ActiveRecording>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl IpcState {
    /// State over an empty project, without third-party plugin hosting.
    pub fn new() -> Self {
        Self::from_control(Control::new())
    }

    /// State over an empty project that hosts third-party plugins through `plugins`.
    pub fn with_plugins(plugins: ow_plugin_host::PluginCache) -> Self {
        Self::from_control(Control::new().with_plugins(plugins))
    }

    fn from_control(control: Control) -> Self {
        Self {
            control: Mutex::new(control),
            subscriptions: Arc::new(Subscriptions::new()),
            bridge: Mutex::new(None),
            peaks: peaks::PeakStore::default(),
            builds: Mutex::new(Vec::new()),
            recording: Mutex::new(None),
        }
    }
}

impl IpcState {
    /// Registers the UI's patch channel (replacing an earlier one) and returns the snapshot it
    /// continues from; both happen under one lock, so no patch falls in between.
    pub fn attach(&self, sink: Box<dyn PatchSink>) -> Snapshot {
        lock(&self.control).attach(sink)
    }

    /// The whole project at the current sequence number.
    pub fn snapshot(&self) -> Snapshot {
        lock(&self.control).snapshot()
    }

    /// Recompiles the project when it holds hosted plugins (the plugin list changed: one that
    /// was missing may be installed now, or one that was playing is gone).
    pub fn refresh_plugins(&self, target: &dyn control::PlanTarget) {
        let mut control = lock(&self.control);
        let hosted = control
            .session()
            .project()
            .devices
            .values()
            .any(|device| ow_plugin_host::is_plugin_kind(&device.kind));
        if hosted {
            control.apply_to_engine(target);
        }
    }

    /// Releases every plugin instance (the app is closing).
    pub fn release_plugins(&self) {
        let plugins = lock(&self.control).plugins().cloned();
        if let Some(plugins) = plugins {
            plugins.clear();
        }
    }

    /// Runs `work` with the control locked: every command inside is ordered after the previous
    /// one with nothing from the UI in between (the MCP `edit` tool applies a list this way).
    pub fn with_control<R>(&self, work: impl FnOnce(&mut Control) -> R) -> R {
        work(&mut lock(&self.control))
    }
}

impl Default for IpcState {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs `work` on Tauri's blocking pool. The commands below lock the control state, compile
/// plans and join threads; none of that may run on an async worker or the main thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| error.to_string())?
}

/// Registers the UI's patch channel and returns the snapshot to continue from.
#[tauri::command]
pub async fn ipc_attach(app: AppHandle, patches: Channel<ModelPatch>) -> Result<Snapshot, String> {
    blocking(move || Ok(app.state::<IpcState>().attach(Box::new(patches)))).await
}

/// The whole project at the current sequence number (the answer to a detected gap).
#[tauri::command]
pub async fn ipc_snapshot(app: AppHandle) -> Result<Snapshot, String> {
    blocking(move || Ok(app.state::<IpcState>().snapshot())).await
}

/// Applies one UI command; the patch it produces arrives on the attached channel.
#[tauri::command]
pub async fn ipc_command(app: AppHandle, command: UiCommand) -> Result<CommandReply, String> {
    blocking(move || {
        let state = app.state::<IpcState>();
        let engine = app.state::<EngineState>();
        lock(&state.control).command(command, engine.engine())
    })
    .await
}

/// Reserves `count` entity ids for the UI's next edits.
#[tauri::command]
pub async fn ipc_alloc_ids(app: AppHandle, count: u32) -> Result<CommandReply, String> {
    blocking(move || {
        let state = app.state::<IpcState>();
        let engine = app.state::<EngineState>();
        lock(&state.control).command(UiCommand::AllocIds { count }, engine.engine())
    })
    .await
}

/// Every device kind with its parameter descriptors: the built-in devices, then the scanned
/// plugins. What the UI builds device panels, the effect picker and the instrument picker from.
/// Needs no control lock.
#[tauri::command]
pub fn device_descriptors(app: AppHandle) -> Vec<ow_ipc::DeviceDescriptor> {
    let plugins = app.try_state::<crate::plugins::Plugins>();
    crate::plugins::all_descriptors(plugins.as_deref())
}

/// Starts a new, empty project and returns it as a snapshot (the patch channel gets a reset).
#[tauri::command]
pub async fn project_new(app: AppHandle) -> Result<Snapshot, String> {
    blocking(move || {
        let state = app.state::<IpcState>();
        let engine = app.state::<EngineState>();
        state.project_new(engine.engine())
    })
    .await
}

/// Opens the `.owave` bundle at `path`.
#[tauri::command]
pub async fn project_open(app: AppHandle, path: String) -> Result<Snapshot, String> {
    blocking(move || {
        let state = app.state::<IpcState>();
        let engine = app.state::<EngineState>();
        state.project_open(Path::new(&path), engine.engine())
    })
    .await
}

/// Saves to `path` (or, without one, to where the project came from); returns the bundle path.
#[tauri::command]
pub async fn project_save(app: AppHandle, path: Option<String>) -> Result<String, String> {
    blocking(move || {
        let state = app.state::<IpcState>();
        let engine = app.state::<EngineState>();
        state.project_save(path.as_deref().map(Path::new), engine.engine())
    })
    .await
}

/// Imports the audio file at `path` into the project's media pool.
#[tauri::command]
pub async fn media_import(app: AppHandle, path: String) -> Result<MediaImportReply, String> {
    blocking(move || {
        let state = app.state::<IpcState>();
        let engine = app.state::<EngineState>();
        state.media_import(Path::new(&path), engine.engine())
    })
    .await
}

/// Renders the project to a WAV file at `path`; returns the path written.
#[tauri::command]
pub async fn export_wav(app: AppHandle, path: String) -> Result<String, String> {
    blocking(move || app.state::<IpcState>().export_wav(Path::new(&path))).await
}

/// Renders the project with options (optional loudness normalisation) to a WAV file; answers
/// what was written and measured.
#[tauri::command]
pub async fn export_mix(
    app: AppHandle,
    options: ow_ipc::ExportOptions,
) -> Result<ow_ipc::ExportResult, String> {
    blocking(move || app.state::<IpcState>().export_mix(&options)).await
}

/// Renders the mix (or one strip, or a beat range) offline and measures it.
#[tauri::command]
pub async fn analyze(
    app: AppHandle,
    request: ow_ipc::AnalysisRequest,
) -> Result<ow_ipc::AnalysisReport, String> {
    blocking(move || app.state::<IpcState>().analyze(&request)).await
}

/// Starts (or restarts) the telemetry bridge: frames go to `frames` as raw bytes at `hz`
/// (at most 60).
#[tauri::command]
pub async fn telemetry_start(
    app: AppHandle,
    frames: Channel<InvokeResponseBody>,
    hz: u32,
) -> Result<(), String> {
    blocking(move || {
        let state = app.state::<IpcState>();
        let engine = app.state::<EngineState>();
        let mut slot = lock(&state.bridge);
        if let Some(mut old) = slot.take() {
            old.stop();
        }
        let bank = Arc::clone(engine.engine().telemetry_bank());
        let midi_source = Arc::clone(engine.engine().midi());
        let mut tick = 0_u32;
        *slot = Some(bridge::Bridge::start(
            bank,
            Arc::clone(&state.subscriptions),
            hz,
            move |bank| {
                // MIDI drops are counted by the hub, not on the audio thread; copy them over
                // about ten times a second.
                if tick.is_multiple_of(6) {
                    bank.set_overflow(Overflow::Midi, midi_source.status().dropped);
                }
                tick = tick.wrapping_add(1);
            },
            move |bytes| frames.send(InvokeResponseBody::Raw(bytes.to_vec())).is_ok(),
        ));
        Ok(())
    })
    .await
}

/// Replaces the set of meter slots the UI can currently see (a few atomic stores).
#[tauri::command]
pub async fn telemetry_set_strips(
    state: State<'_, IpcState>,
    slots: Vec<u16>,
) -> Result<(), String> {
    state.subscriptions.set(&slots);
    Ok(())
}

/// Stops the telemetry bridge (joins its thread).
#[tauri::command]
pub async fn telemetry_stop(app: AppHandle) -> Result<(), String> {
    blocking(move || {
        if let Some(mut bridge) = lock(&app.state::<IpcState>().bridge).take() {
            bridge.stop();
        }
        Ok(())
    })
    .await
}

/// `bins` min/max pairs over frames `[start, end)` of a media item, as raw bytes (layout in
/// `ow_ipc::peaks`). Served from the peak cache.
#[tauri::command]
pub async fn peaks_range(
    app: AppHandle,
    media: u64,
    start: u64,
    end: u64,
    bins: u32,
) -> Result<Response, String> {
    blocking(move || {
        Ok(Response::new(
            app.state::<IpcState>().peaks.range(media, start, end, bins),
        ))
    })
    .await
}
