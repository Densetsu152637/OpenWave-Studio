//! The MCP server inside the app (slice M7): the tools, wired to the control layer, and the
//! lifecycle of the local listener.
//!
//! `ow-mcp` owns the protocol, the tool catalog and the socket. This module implements
//! [`ToolBackend`] over the same plain `IpcState` / `EngineState` methods the Tauri commands use,
//! so every edit goes through `Control` and `Session`: the UI mirror receives the patches, undo
//! works, and the engine recompiles exactly as for a UI edit.
//!
//! # Parity (M-G15)
//!
//! An agent must be able to do anything a user can do, except generate music. [`ROUTES`] maps
//! every registered Tauri command (see `COMMANDS` in `lib.rs`) to the tool that does the same,
//! [`UI_ROUTES`] does the same for every `UiCommand` variant, and the `edit` tool's schema
//! covers every `EditCommand` variant. The `mcp_parity` tests fail when a command or variant is
//! added without a route or an explicit, reasoned exclusion. A slice that adds a UI feature
//! adds its tool (or tool argument) in the same slice.
//!
//! # Plugins (M8, M9)
//!
//! `scan_plugins` and `list_plugins` drive the plugin registry (`crate::plugins`);
//! `get_device_types` lists the scanned plugins next to the built-in devices; a plugin's
//! parameters are edited with the `edit` tool's `set_device_param` like any device's. The
//! `plugin_editor` tool opens and closes a hosted plugin's own editor window (Windows and macOS;
//! Linux answers "unavailable"); what the user does in that window reaches the project as
//! `set_device_param` edits, so `undo`, `get_project` and this server see them.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ow_engine::{Engine, TransportCommand};
use ow_ipc::{
    AnalysisRequest, CommandReply, EditCommand, ExportOptions, ModelPatch, NormalizeOptions,
    PatchOp, PluginScanSummary, Snapshot, UiCommand,
};
use ow_mcp::listener::{McpListener, announce_disabled};
use ow_mcp::tools::{
    AnalyzeArgs, AudioAction, AudioSettingsArgs, EditArgs, EditorAction, ExportMidiArgs,
    ExportWavArgs, ImportAudioArgs, ImportMidiArgs, MidiEditArgs, PluginEditorArgs, ProjectAction,
    ProjectArgs, RecordAction, RecordArgs, TransportAction, TransportArgs,
};
use ow_mcp::{Server, ToolBackend, ToolCall, ToolError, ToolOutput, endpoint};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

use crate::engine::{EngineState, send_transport};
use crate::ipc::IpcState;
use crate::ipc::control::Control;
use crate::ipc::export::wav_path;
use crate::ipc::project::bundle_path;
use crate::paths::Paths;
use crate::plugins::Plugins;
use crate::prefs::Preferences;

/// Most patch operations an `edit` answer lists (the total is always reported).
const MAX_LISTED_OPS: usize = 100;

/// What the tools need from the app: the shared state the Tauri commands use.
pub trait Host: Send + Sync + 'static {
    /// The project, patch channel and file operations.
    fn ipc(&self) -> &IpcState;
    /// The audio engine, preferences and device lists.
    fn engine_state(&self) -> &EngineState;
    /// The plugin registry.
    fn plugins(&self) -> &Plugins;
    /// Tells the UI that the plugin list changed. The default does nothing (no UI in tests).
    fn plugins_changed(&self) {}
}

/// The host in the running app: Tauri's managed state.
pub struct AppHost(pub AppHandle);

impl Host for AppHost {
    fn ipc(&self) -> &IpcState {
        self.0.state::<IpcState>().inner()
    }

    fn engine_state(&self) -> &EngineState {
        self.0.state::<EngineState>().inner()
    }

    fn plugins(&self) -> &Plugins {
        self.0.state::<Plugins>().inner()
    }

    fn plugins_changed(&self) {
        use tauri::Emitter as _;
        let _ = self.0.emit(crate::plugins::PLUGINS_CHANGED, ());
    }
}

/// The tools, over a host.
pub struct AppBackend {
    host: Arc<dyn Host>,
}

impl AppBackend {
    /// A backend for `host`.
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self { host }
    }

    /// An MCP server over this backend.
    pub fn server(host: Arc<dyn Host>) -> Arc<Server> {
        Arc::new(Server::new(
            Arc::new(Self::new(host)),
            crate::cli::effective_version(),
        ))
    }

    fn ipc(&self) -> &IpcState {
        self.host.ipc()
    }

    fn engine_state(&self) -> &EngineState {
        self.host.engine_state()
    }

    fn plugins(&self) -> &Plugins {
        self.host.plugins()
    }
}

impl ToolBackend for AppBackend {
    fn call(&self, call: ToolCall) -> Result<ToolOutput, ToolError> {
        let output = match call {
            ToolCall::GetProject(_) => Ok(self.get_project()),
            ToolCall::GetDeviceTypes(_) => Ok(self.device_types()),
            ToolCall::Edit(args) => self.edit(args),
            ToolCall::AllocIds(args) => self.alloc_ids(args.count),
            ToolCall::Undo(_) => self.history_step(UiCommand::Undo, "Undid the latest step"),
            ToolCall::Redo(_) => self.history_step(UiCommand::Redo, "Redid the latest step"),
            ToolCall::Transport(args) => self.transport(&args),
            ToolCall::Project(args) => self.project(&args),
            ToolCall::ImportAudio(args) => self.import_audio(&args),
            ToolCall::Record(args) => self.record(&args),
            ToolCall::MidiEdit(args) => self.midi_edit(&args),
            ToolCall::ImportMidi(args) => self.import_midi(&args),
            ToolCall::ExportMidi(args) => self.export_midi(&args),
            ToolCall::Analyze(args) => self.analyze(args),
            ToolCall::ExportWav(args) => self.export_wav(args),
            ToolCall::AudioSettings(args) => self.audio_settings(&args),
            ToolCall::ScanPlugins(args) => Ok(self.scan_plugins(args.force)),
            ToolCall::ListPlugins(_) => Ok(self.list_plugins()),
            ToolCall::PluginEditor(args) => self.plugin_editor(&args),
        };
        output.map_err(ToolError)
    }
}

/// The answer of `plugin_editor` on a platform without editor windows.
fn plugin_editor_unavailable() -> ToolOutput {
    ToolOutput::new(
        json!({"available": false, "tool": "plugin_editor", "reason": "plugin editor windows are not available on this platform (Linux); edit the plugin's parameters with the edit tool's set_device_param"}),
        "plugin_editor is unavailable: plugin editor windows are not available on this platform. Set the plugin's parameters with the edit tool (set_device_param).",
    )
}

/// The text of a scan or list answer.
fn scan_line(summary: &PluginScanSummary) -> String {
    let mut line = format!(
        "{} plugins usable in {} plugin files ({} scanned now, the rest from the cache).",
        summary.plugins.len(),
        summary.files,
        summary.scanned
    );
    if !summary.failures.is_empty() {
        let failed: Vec<String> = summary
            .failures
            .iter()
            .take(5)
            .map(|failure| format!("{} ({})", failure.path, failure.status))
            .collect();
        line.push_str(&format!(
            " {} files were skipped: {}.",
            summary.failures.len(),
            failed.join(", ")
        ));
    }
    line
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

/// A one-line description of a project for the text summaries.
fn project_line(snapshot: &Snapshot) -> String {
    let project = &snapshot.project;
    format!(
        "\"{}\": {} tracks, {} buses, {} clips, {} media items; {}; saved at {}",
        project.name,
        project.tracks.len(),
        project.buses.len(),
        project.clips.len(),
        project.media_pool.len(),
        if snapshot.history.dirty {
            "unsaved changes"
        } else {
            "no unsaved changes"
        },
        snapshot.project_path.as_deref().unwrap_or("(never saved)"),
    )
}

/// `{op, id}` for one patch operation.
fn op_summary(op: &PatchOp) -> Value {
    let Ok(Value::Object(fields)) = serde_json::to_value(op) else {
        return Value::Null;
    };
    let kind = fields.get("op").cloned().unwrap_or(Value::Null);
    let id = fields
        .get("id")
        .cloned()
        .or_else(|| fields.values().find_map(|value| value.get("id").cloned()));
    match id {
        Some(id) => json!({"op": kind, "id": id}),
        None => json!({"op": kind}),
    }
}

/// What a list of patches changed, for the answer to an `edit`, `undo` or `redo`.
fn patch_report(patches: &[ModelPatch], reply_seq: u64, engine_error: Option<String>) -> Value {
    let ops: Vec<Value> = patches
        .iter()
        .flat_map(|patch| patch.ops.iter().map(op_summary))
        .collect();
    let total = ops.len();
    let history = patches
        .last()
        .map(|patch| to_json(&patch.history).unwrap_or(Value::Null));
    json!({
        "seq": reply_seq,
        "ops_total": total,
        "ops": ops.into_iter().take(MAX_LISTED_OPS).collect::<Vec<_>>(),
        "history": history,
        "engine_error": engine_error,
    })
}

impl AppBackend {
    fn get_project(&self) -> ToolOutput {
        let snapshot = self.ipc().snapshot();
        let summary = format!("Project {}.", project_line(&snapshot));
        match to_json(&snapshot) {
            Ok(value) => ToolOutput::new(value, summary),
            Err(error) => ToolOutput::new(json!({"error": error}), "could not encode the project"),
        }
    }

    /// The device kinds: built-in ones and the scanned plugins.
    fn device_types(&self) -> ToolOutput {
        let builtin = ow_ipc::device_descriptors();
        let plugins = self.plugins().descriptors();
        let summary = format!(
            "{} built-in device kinds and {} scanned plugins (use scan_plugins to look for more).",
            builtin.len(),
            plugins.len()
        );
        ToolOutput::new(
            json!({"builtin": builtin, "plugins": plugins, "plugins_available": true}),
            summary,
        )
    }

    /// Scans the plugin folders (the standard ones and the preferences' extra folders).
    fn scan_plugins(&self, force: bool) -> ToolOutput {
        let extra = self.engine_state().preferences().paths.plugin_dirs;
        let summary = self.plugins().scan(&extra, force);
        self.ipc().refresh_plugins(self.engine_state().engine());
        self.host.plugins_changed();
        let text = scan_line(&summary);
        ToolOutput::new(to_json(&summary).unwrap_or(Value::Null), text)
    }

    /// The plugins the last scan found and the files it gave up on.
    fn list_plugins(&self) -> ToolOutput {
        let summary = self.plugins().summary();
        let text = scan_line(&summary);
        ToolOutput::new(to_json(&summary).unwrap_or(Value::Null), text)
    }

    /// Opens or closes the editor window of a hosted plugin.
    fn plugin_editor(&self, args: &PluginEditorArgs) -> Result<ToolOutput, String> {
        if !ow_plugin_host::editor::AVAILABLE {
            return Ok(plugin_editor_unavailable());
        }
        let device = args.device;
        match args.action {
            EditorAction::Open => {
                let (width, height) = self.plugins().open_editor(device)?;
                Ok(ToolOutput::new(
                    json!({"available": true, "action": "open", "device": device, "open": true, "width": width, "height": height}),
                    format!(
                        "Opened the editor of device {device} ({width} x {height} pixels). Changes made in it arrive as set_device_param edits."
                    ),
                ))
            }
            EditorAction::Close => {
                let was_open = self.plugins().close_editor(device)?;
                Ok(ToolOutput::new(
                    json!({"available": true, "action": "close", "device": device, "open": false, "was_open": was_open}),
                    if was_open {
                        format!("Closed the editor of device {device}.")
                    } else {
                        format!("The editor of device {device} was not open.")
                    },
                ))
            }
        }
    }

    /// Applies the commands under one lock, as one undo step.
    fn edit(&self, args: EditArgs) -> Result<ToolOutput, String> {
        let count = args.commands.len();
        if count == 0 {
            return Err("edit needs at least one command".into());
        }
        let label = args.label.unwrap_or_else(|| "AI edit".into());
        let engine = self.engine_state().engine();
        let (patches, reply) = self
            .ipc()
            .with_control(|control| apply_edits(control, args.commands, label, engine))?;
        let report = patch_report(&patches, reply.seq, reply.engine_error);
        let kinds: Vec<String> = patches
            .iter()
            .flat_map(|patch| patch.ops.iter())
            .map(|op| op_summary(op)["op"].as_str().unwrap_or("?").to_owned())
            .collect();
        let summary = format!(
            "Applied {count} command{} (project sequence {}); model changes: {}.",
            if count == 1 { "" } else { "s" },
            reply.seq,
            if kinds.is_empty() {
                "none".to_owned()
            } else {
                let mut shown: Vec<String> = kinds.iter().take(8).cloned().collect();
                if kinds.len() > 8 {
                    shown.push(format!("and {} more", kinds.len() - 8));
                }
                shown.join(", ")
            },
        );
        Ok(ToolOutput::new(report, summary))
    }

    fn alloc_ids(&self, count: u32) -> Result<ToolOutput, String> {
        let engine = self.engine_state().engine();
        let reply = self
            .ipc()
            .with_control(|control| control.command(UiCommand::AllocIds { count }, engine))?;
        let summary = format!("Reserved {} ids.", reply.ids.len());
        Ok(ToolOutput::new(
            json!({"ids": reply.ids, "seq": reply.seq}),
            summary,
        ))
    }

    fn history_step(&self, command: UiCommand, done: &str) -> Result<ToolOutput, String> {
        let engine = self.engine_state().engine();
        let (result, patch) = self
            .ipc()
            .with_control(|control| control.command_traced(command, engine));
        let reply = result?;
        let patches: Vec<ModelPatch> = patch.into_iter().collect();
        let report = patch_report(&patches, reply.seq, reply.engine_error);
        Ok(ToolOutput::new(
            report,
            format!("{done} (project sequence {}).", reply.seq),
        ))
    }

    fn transport(&self, args: &TransportArgs) -> Result<ToolOutput, String> {
        let engine_state = self.engine_state();
        match args.action {
            TransportAction::Play => send_transport(engine_state, TransportCommand::Play)?,
            TransportAction::Stop => self.ipc().transport_stop(engine_state.engine())?,
            TransportAction::Locate => {
                let beats = args
                    .beats
                    .filter(|beats| beats.is_finite() && *beats >= 0.0)
                    .ok_or("locate needs `beats`, a number of 0 or more")?;
                send_transport(engine_state, TransportCommand::LocateBeats(beats))?;
            }
            TransportAction::Status => {}
        }
        let moving = args.action == TransportAction::Status;
        let target = match args.action {
            TransportAction::Locate => args.beats,
            _ => None,
        };
        let (state, summary) = self.transport_state(moving, target);
        let verb = match args.action {
            TransportAction::Play => "Playing.",
            TransportAction::Stop => "Stopped.",
            TransportAction::Locate => "Moved the position.",
            TransportAction::Status => "Transport status.",
        };
        Ok(ToolOutput::new(state, format!("{verb} {summary}")))
    }

    /// The engine status and the transport position. With `measure_motion` also whether the
    /// transport is playing or counting in, from the flags the audio thread publishes (a locate
    /// while stopped is not playing).
    /// With `target_beats` (after a locate) it waits up to about 100 ms for the engine to reach
    /// that position, and reports the requested target when it has not yet.
    fn transport_state(&self, measure_motion: bool, target_beats: Option<f64>) -> (Value, String) {
        let engine_state = self.engine_state();
        let bank = engine_state.engine().telemetry_bank();
        let (rate, tempo) = self.ipc().with_control(|control| {
            let project = control.session().project();
            (project.settings.sample_rate, project.tempo_map.clone())
        });
        let mut confirmed = true;
        let mut requested = None;
        if let Some(beats) = target_beats {
            let want = ow_model::time::beats_to_sample_index(&tempo, beats, rate);
            let close = u64::from(rate) / 50;
            let deadline = std::time::Instant::now() + Duration::from_millis(100);
            while bank.position().abs_diff(want) > close {
                if std::time::Instant::now() >= deadline {
                    confirmed = false;
                    requested = Some(beats);
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let position = bank.position();
        let beats = requested.unwrap_or_else(|| tempo.samples_to_beats(position as f64, rate));
        let status = engine_state.status();
        let moving = measure_motion.then_some(status.moving || status.counting_in);
        let summary = format!(
            "Position {beats:.3} beats{}; audio {}{}.",
            if confirmed {
                ""
            } else {
                " (requested; the engine has not confirmed it yet)"
            },
            status.audio_state,
            match moving {
                Some(true) => ", playing",
                Some(false) => ", not playing",
                None => "",
            },
        );
        (
            json!({
                "position_frames": position,
                "position_beats": beats,
                "position_confirmed": confirmed,
                "sample_rate": rate,
                "moving": moving,
                "counting_in": status.counting_in,
                "recording": status.recording,
                "engine": to_json(&status).unwrap_or(Value::Null),
            }),
            summary,
        )
    }

    fn project(&self, args: &ProjectArgs) -> Result<ToolOutput, String> {
        let engine = self.engine_state().engine();
        let ipc = self.ipc();
        let refuse_unsaved = |what: &str| -> Result<(), String> {
            let dirty = ipc.with_control(|control| control.session().editor().is_dirty());
            if dirty && !args.discard_changes {
                return Err(format!(
                    "the open project has unsaved changes; {what} would discard them. Save first, or pass discard_changes=true"
                ));
            }
            Ok(())
        };
        let need_path = || {
            args.path
                .as_deref()
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| format!("{:?} needs `path`", args.action))
        };
        match args.action {
            ProjectAction::New => {
                refuse_unsaved("a new project")?;
                let snapshot = ipc.project_new(engine)?;
                Ok(summarise_snapshot("Started a new project", &snapshot))
            }
            ProjectAction::Open => {
                let path = need_path()?;
                refuse_unsaved("opening another project")?;
                let snapshot = ipc.project_open(Path::new(path), engine)?;
                Ok(summarise_snapshot("Opened the project", &snapshot))
            }
            ProjectAction::Save => {
                let saved = ipc.project_save(None, engine)?;
                Ok(ToolOutput::new(
                    json!({"path": saved}),
                    format!("Saved to {saved}."),
                ))
            }
            ProjectAction::SaveAs => {
                let path = need_path()?;
                let target = bundle_path(Path::new(path));
                let current =
                    ipc.with_control(|control| control.session().project_path().map(str::to_owned));
                let same = current.as_deref().map(Path::new) == Some(target.as_path());
                if target.exists() && !same {
                    // Only an existing OpenWave project may be replaced, never another file or
                    // folder (saving writes into the folder).
                    if !target.join(ow_model::PROJECT_FILE).is_file() {
                        return Err(format!(
                            "{} exists and is not an OpenWave project bundle; choose another path",
                            target.display()
                        ));
                    }
                    if !args.overwrite {
                        return Err(format!(
                            "{} already exists; pass overwrite=true to replace it",
                            target.display()
                        ));
                    }
                }
                let saved = ipc.project_save(Some(Path::new(path)), engine)?;
                Ok(ToolOutput::new(
                    json!({"path": saved}),
                    format!("Saved to {saved}."),
                ))
            }
        }
    }

    fn import_audio(&self, args: &ImportAudioArgs) -> Result<ToolOutput, String> {
        let engine = self.engine_state().engine();
        let reply = self.ipc().media_import(Path::new(&args.path), engine)?;
        let summary = format!(
            "Imported as media {} ({} frames at {} Hz, {} channels). Add a clip that references it with the edit tool.",
            reply.media, reply.frames, reply.sample_rate, reply.channels
        );
        Ok(ToolOutput::new(to_json(&reply)?, summary))
    }

    fn record(&self, args: &RecordArgs) -> Result<ToolOutput, String> {
        let engine = self.engine_state().engine();
        let ipc = self.ipc();
        match args.action {
            RecordAction::Arm => {
                let track = args
                    .track
                    .ok_or("arm needs `track`, an audio or MIDI track id")?;
                let armed = args.armed.unwrap_or(true);
                ipc.record_set_arm(track, armed)?;
                Ok(ToolOutput::new(
                    json!({"track": track, "armed": armed}),
                    format!(
                        "Track {track} {}.",
                        if armed { "armed" } else { "disarmed" }
                    ),
                ))
            }
            RecordAction::Start => {
                ipc.record_start_with(engine, args.midi_mode)?;
                Ok(ToolOutput::new(
                    json!({"recording": true}),
                    "Recording started.",
                ))
            }
            RecordAction::Stop => {
                ipc.transport_stop(engine)?;
                Ok(ToolOutput::new(
                    json!({"recording": false}),
                    "Stopped; any takes were added to the project as clips.",
                ))
            }
        }
    }

    fn midi_edit(&self, args: &MidiEditArgs) -> Result<ToolOutput, String> {
        let reply = self.ipc().midi_edit(
            args.clip,
            &args.edit,
            args.notes.as_deref(),
            self.engine_state().engine(),
        )?;
        let summary = format!(
            "{} on clip {}: {} notes before, {} after.",
            args.edit.label(),
            reply.clip,
            reply.notes_before,
            reply.notes_after
        );
        Ok(ToolOutput::new(to_json(&reply)?, summary))
    }

    fn import_midi(&self, args: &ImportMidiArgs) -> Result<ToolOutput, String> {
        let reply = self.ipc().midi_import(
            Path::new(&args.path),
            args.track,
            args.start_beat.unwrap_or(0.0),
            args.apply_tempo,
            self.engine_state().engine(),
        )?;
        let summary = format!(
            "Imported {} clip(s) ({} new track(s)){}.",
            reply.clips.len(),
            reply.tracks.len(),
            if reply.tempo_applied {
                ", tempo map replaced"
            } else {
                ""
            }
        );
        Ok(ToolOutput::new(to_json(&reply)?, summary))
    }

    fn export_midi(&self, args: &ExportMidiArgs) -> Result<ToolOutput, String> {
        let reply = self.ipc().midi_export(
            Path::new(&args.path),
            &args.clips,
            &args.tracks,
            args.overwrite,
        )?;
        let summary = format!(
            "Wrote {} ({} track(s), {} notes).",
            reply.path, reply.tracks, reply.notes
        );
        Ok(ToolOutput::new(to_json(&reply)?, summary))
    }

    fn analyze(&self, args: AnalyzeArgs) -> Result<ToolOutput, String> {
        let report = self.ipc().analyze(&AnalysisRequest {
            strip: args.strip,
            start_beat: args.start_beat,
            end_beat: args.end_beat,
        })?;
        let lufs = |value: Option<f64>| value.map_or("silence".to_owned(), |v| format!("{v:.1}"));
        let summary = format!(
            "Integrated {} LUFS, true peak {} dBTP, sample peak {} dBFS, RMS {} dBFS, correlation {:.2}.",
            lufs(report.integrated_lufs),
            lufs(report.true_peak_dbtp),
            lufs(report.sample_peak_db),
            lufs(report.rms_db),
            report.correlation
        );
        Ok(ToolOutput::new(to_json(&report)?, summary))
    }

    fn export_wav(&self, args: ExportWavArgs) -> Result<ToolOutput, String> {
        if !Path::new(&args.path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
        {
            return Err(
                "the export path must end in .wav; export_wav never writes other file types".into(),
            );
        }
        let target = wav_path(Path::new(&args.path));
        if target.exists() && !args.overwrite {
            return Err(format!(
                "{} already exists; pass overwrite=true to replace it",
                target.display()
            ));
        }
        let result = self.ipc().export_mix(&ExportOptions {
            path: args.path,
            normalize: args.normalize.map(|normalize| NormalizeOptions {
                target_lufs: normalize.target_lufs,
                true_peak_ceiling_db: normalize.true_peak_ceiling_db,
            }),
        })?;
        let measured = result
            .analysis
            .integrated_lufs
            .map_or("silence".to_owned(), |v| format!("{v:.1} LUFS"));
        let peak = result
            .analysis
            .true_peak_dbtp
            .map_or("silence".to_owned(), |v| format!("{v:.1} dBTP"));
        let applied = result.normalization.as_ref().map_or(String::new(), |n| {
            format!(" Normalisation applied {:.2} dB.", n.gain_db)
        });
        let summary = format!(
            "Wrote {} ({} frames at {} Hz): {measured}, true peak {peak}.{applied}",
            result.path, result.frames, result.sample_rate
        );
        Ok(ToolOutput::new(to_json(&result)?, summary))
    }

    fn audio_settings(&self, args: &AudioSettingsArgs) -> Result<ToolOutput, String> {
        let state = self.engine_state();
        match args.action {
            AudioAction::ListDevices => {
                let lists = state.device_lists()?;
                let devices: usize = lists
                    .backends
                    .iter()
                    .map(|backend| backend.devices.len())
                    .sum();
                Ok(ToolOutput::new(
                    to_json(&lists)?,
                    format!(
                        "{} audio backends with {devices} devices; {} MIDI inputs.",
                        lists.backends.len(),
                        lists.midi_inputs.len()
                    ),
                ))
            }
            AudioAction::Get => {
                let snapshot = state.prefs_snapshot();
                Ok(ToolOutput::new(
                    to_json(&snapshot)?,
                    format!("Preferences are stored in {}.", snapshot.config_file),
                ))
            }
            AudioAction::Set => {
                let patch = args
                    .preferences
                    .as_ref()
                    .filter(|patch| patch.is_object())
                    .ok_or("set needs `preferences`, an object with the fields to change")?;
                let current = state.preferences();
                let next = merge_preferences(&current, patch)?;
                if next.ai != current.ai {
                    return Err(
                        "AI control can only be changed by the user, in OpenWave Studio's settings"
                            .into(),
                    );
                }
                let applied = state.apply(next)?;
                let summary = match &applied.audio_error {
                    Some(error) => {
                        format!("Preferences stored, but the audio device was not applied: {error}")
                    }
                    None => "Preferences stored and applied.".to_owned(),
                };
                Ok(ToolOutput::new(to_json(&applied)?, summary))
            }
            AudioAction::Retry => {
                let status = state.retry_audio();
                Ok(ToolOutput::new(
                    to_json(&status)?,
                    "Retrying the audio device.",
                ))
            }
            AudioAction::TestTone => {
                let on = args.on.ok_or("test_tone needs `on` (true or false)")?;
                let status = state.test_tone(on);
                Ok(ToolOutput::new(
                    to_json(&status)?,
                    format!("Test tone {}.", if status.test_tone { "on" } else { "off" }),
                ))
            }
        }
    }
}

fn summarise_snapshot(done: &str, snapshot: &Snapshot) -> ToolOutput {
    ToolOutput::new(
        json!({
            "name": snapshot.project.name,
            "path": snapshot.project_path,
            "tracks": snapshot.project.tracks.len(),
            "buses": snapshot.project.buses.len(),
            "clips": snapshot.project.clips.len(),
            "dirty": snapshot.history.dirty,
            "seq": snapshot.seq,
        }),
        format!("{done}: {}.", project_line(snapshot)),
    )
}

/// RFC 7386 merge patch of `patch` into the stored preferences; the result must still be valid
/// preferences.
fn merge_preferences(current: &Preferences, patch: &Value) -> Result<Preferences, String> {
    fn merge(into: &mut Value, patch: &Value) {
        match patch {
            Value::Object(fields) => {
                if !into.is_object() {
                    *into = json!({});
                }
                let Value::Object(target) = into else {
                    return;
                };
                for (key, value) in fields {
                    if value.is_null() {
                        target.remove(key);
                    } else {
                        merge(target.entry(key.clone()).or_insert(Value::Null), value);
                    }
                }
            }
            other => *into = other.clone(),
        }
    }
    let mut value = to_json(current)?;
    merge(&mut value, patch);
    serde_json::from_value(value).map_err(|error| format!("the preferences are not valid: {error}"))
}

/// Runs `commands` in order under the caller's control lock. More than one command is wrapped in
/// a transaction (one undo step, reverted entirely when a command is refused). Returns the
/// patches the commands produced (the UI received them too) and the last reply.
fn apply_edits(
    control: &mut Control,
    commands: Vec<EditCommand>,
    label: String,
    engine: &Engine,
) -> Result<(Vec<ModelPatch>, CommandReply), String> {
    let count = commands.len();
    let transaction = count > 1;
    let mut patches = Vec::new();
    if transaction {
        let (begun, patch) = control.command_traced(UiCommand::Begin { label }, engine);
        begun?;
        patches.extend(patch);
    }
    let mut reply = None;
    for (index, edit) in commands.into_iter().enumerate() {
        let (result, patch) = control.command_traced(UiCommand::Edit { edit }, engine);
        patches.extend(patch);
        match result {
            Ok(done) => reply = Some(done),
            Err(error) if transaction => {
                let _ = control.command_traced(UiCommand::Cancel, engine);
                return Err(format!(
                    "command {} of {count} was refused ({error}); nothing was applied",
                    index + 1
                ));
            }
            Err(error) => return Err(error),
        }
    }
    if transaction {
        let (ended, patch) = control.command_traced(UiCommand::End, engine);
        reply = Some(ended?);
        patches.extend(patch);
    }
    let reply = reply.ok_or("edit needs at least one command")?;
    Ok((patches, reply))
}

/// The tool that does what each registered Tauri command does (`COMMANDS` in `lib.rs`). A
/// command that is not here must be in the exclusion list of the `mcp_parity` test.
pub const ROUTES: &[(&str, &str)] = &[
    ("engine_status", "transport"),
    ("set_test_tone", "audio_settings"),
    ("transport_play", "transport"),
    ("transport_stop", "transport"),
    ("transport_locate", "transport"),
    ("prefs_get", "audio_settings"),
    ("prefs_set", "audio_settings"),
    ("audio_devices", "audio_settings"),
    ("audio_retry", "audio_settings"),
    ("ipc_snapshot", "get_project"),
    ("ipc_command", "edit"),
    ("ipc_alloc_ids", "alloc_ids"),
    ("device_descriptors", "get_device_types"),
    ("plugins_scan", "scan_plugins"),
    ("plugins_list", "list_plugins"),
    ("plugin_editor_open", "plugin_editor"),
    ("plugin_editor_close", "plugin_editor"),
    ("project_new", "project"),
    ("project_open", "project"),
    ("project_save", "project"),
    ("media_import", "import_audio"),
    ("export_wav", "export_wav"),
    ("export_mix", "export_wav"),
    ("analyze", "analyze"),
    ("record_set_arm", "record"),
    ("record_start", "record"),
    ("midi_edit", "midi_edit"),
    ("midi_import", "import_midi"),
    ("midi_export", "export_midi"),
];

/// The tool that does what each `UiCommand` variant does. `begin`, `end` and `cancel` are the
/// transaction an `edit` call with several commands wraps itself in.
pub const UI_ROUTES: &[(&str, &str)] = &[
    ("edit", "edit"),
    ("undo", "undo"),
    ("redo", "redo"),
    ("begin", "edit"),
    ("end", "edit"),
    ("cancel", "edit"),
    ("alloc_ids", "alloc_ids"),
];

/// Tells the world that "Allow AI control" changed (or is as stored): starts or stops the
/// listener to match the preferences.
pub fn sync_with_preferences(app: &AppHandle) {
    let enabled = app.state::<EngineState>().preferences().ai.allow_control;
    let dir = endpoint::dir_in(&app.state::<Paths>().data_dir);
    app.state::<McpState>()
        .sync(enabled, &dir, || Arc::new(AppHost(app.clone())));
}

/// Stops the listener (the app is closing).
pub fn shutdown(app: &AppHandle) {
    app.state::<McpState>().stop();
}

/// Managed state: the running listener, if any.
#[derive(Default)]
pub struct McpState {
    listener: Mutex<Option<McpListener>>,
}

impl McpState {
    /// Makes the listener match `enabled`: starts it (announcing the endpoint in `dir`) or stops
    /// it and announces that AI control is off. `host` builds the host the tools run against.
    pub fn sync(&self, enabled: bool, dir: &Path, host: impl FnOnce() -> Arc<dyn Host>) {
        let mut slot = self
            .listener
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if enabled {
            if slot.is_some() {
                return;
            }
            match McpListener::start(dir, AppBackend::server(host())) {
                Ok(listener) => {
                    eprintln!("[openwave] MCP listening on {}", listener.endpoint());
                    *slot = Some(listener);
                }
                Err(error) => eprintln!("[openwave] MCP listener could not start: {error}"),
            }
        } else {
            if let Some(listener) = slot.take() {
                listener.stop();
            }
            if let Err(error) = announce_disabled(dir) {
                eprintln!("[openwave] could not record that MCP is off: {error}");
            }
        }
    }

    /// Stops the listener and removes its announcement.
    pub fn stop(&self) {
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(listener) = listener {
            listener.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::f64::consts::TAU;
    use std::path::PathBuf;

    use ow_ipc::{Mirror, UiCommand};
    use ow_mcp::client::{AppStream, ClientConfig, authenticate, connect, open};
    use ow_mcp::tools::{TOOL_NAMES, catalog};
    use ow_model::{Clip, ClipContent, MediaId, Owned, Strip, Track, TrackId, TrackKind};
    use strum::VariantNames as _;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};

    use super::*;
    use crate::ipc::control::PatchSink;

    // ---- parity (M-G15) ------------------------------------------------------------------

    /// Commands that have no MCP tool, each with the reason.
    const EXCLUDED_COMMANDS: &[(&str, &str)] = &[
        (
            "ipc_attach",
            "registers the webview's patch channel; an agent reads the same state with get_project and the patch summary of every edit",
        ),
        (
            "telemetry_start",
            "streams meter frames to the webview at 60 Hz (plumbing); an agent measures with analyze and transport status",
        ),
        (
            "telemetry_set_strips",
            "chooses which meters the webview receives (plumbing)",
        ),
        ("telemetry_stop", "stops the telemetry stream (plumbing)"),
        (
            "peaks_range",
            "raw waveform bytes for drawing; analyze gives the measurements an agent can use",
        ),
        (
            "update_status",
            "the updater: an agent must not check for, install or restart into updates",
        ),
        ("update_check", "the updater (see update_status)"),
        ("update_set_channel", "the updater (see update_status)"),
        ("update_restart", "the updater (see update_status)"),
        ("update_install_now", "the updater (see update_status)"),
    ];

    fn sources(dir: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(std::fs::read_to_string(&path).unwrap());
            }
        }
    }

    /// The names inside `tauri::generate_handler![ ... ]` in `lib.rs` (last path segment).
    fn registered_commands() -> BTreeSet<String> {
        let lib = include_str!("lib.rs");
        let marker = "generate_handler![";
        let start = lib.find(marker).expect("generate_handler!") + marker.len();
        let end = start + lib[start..].find(']').unwrap();
        lib[start..end]
            .split(',')
            .map(|path| path.trim().rsplit("::").next().unwrap().trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect()
    }

    /// The functions marked `#[tauri::command]` anywhere in the crate.
    fn defined_commands() -> BTreeSet<String> {
        let mut files = Vec::new();
        sources(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut names = BTreeSet::new();
        for text in files {
            let lines: Vec<&str> = text.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                if line.trim() != "#[tauri::command]" {
                    continue;
                }
                let signature = lines[index + 1..]
                    .iter()
                    .find(|line| line.contains("fn "))
                    .expect("a function after #[tauri::command]");
                let after = signature.split("fn ").nth(1).unwrap();
                names.insert(after.split(['(', '<']).next().unwrap().trim().to_owned());
            }
        }
        names
    }

    #[test]
    fn mcp_parity_commands_const_matches_the_registration() {
        let listed: BTreeSet<String> = crate::COMMANDS
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(
            listed.len(),
            crate::COMMANDS.len(),
            "COMMANDS has a duplicate"
        );
        assert_eq!(
            listed,
            registered_commands(),
            "COMMANDS and generate_handler! in lib.rs must list the same commands"
        );
        assert_eq!(
            listed,
            defined_commands(),
            "every #[tauri::command] must be registered and listed in COMMANDS"
        );
    }

    #[test]
    fn mcp_parity_every_command_has_a_tool_or_a_reasoned_exclusion() {
        let tools: BTreeSet<&str> = TOOL_NAMES.iter().copied().collect();
        let routed: BTreeSet<&str> = ROUTES.iter().map(|(command, _)| *command).collect();
        let excluded: BTreeSet<&str> = EXCLUDED_COMMANDS
            .iter()
            .map(|(command, _)| *command)
            .collect();
        for command in crate::COMMANDS {
            assert!(
                routed.contains(command) ^ excluded.contains(command),
                "command `{command}` needs exactly one of: a route in mcp::ROUTES, an exclusion with a reason"
            );
        }
        for (command, tool) in ROUTES {
            assert!(
                crate::COMMANDS.contains(command),
                "route for unknown command `{command}`"
            );
            assert!(
                tools.contains(tool),
                "route `{command}` points at missing tool `{tool}`"
            );
        }
        for (command, reason) in EXCLUDED_COMMANDS {
            assert!(
                crate::COMMANDS.contains(command),
                "exclusion of unknown command `{command}`"
            );
            assert!(reason.len() > 10, "exclusion of `{command}` needs a reason");
        }
    }

    #[test]
    fn mcp_parity_every_ui_command_variant_has_a_tool() {
        let tools: BTreeSet<&str> = TOOL_NAMES.iter().copied().collect();
        let variants: BTreeSet<&str> = UiCommand::VARIANTS.iter().copied().collect();
        let routed: BTreeSet<&str> = UI_ROUTES.iter().map(|(variant, _)| *variant).collect();
        assert_eq!(
            variants, routed,
            "UiCommand variants and mcp::UI_ROUTES differ"
        );
        for (variant, tool) in UI_ROUTES {
            assert!(
                tools.contains(tool),
                "`{variant}` points at missing tool `{tool}`"
            );
        }
    }

    #[test]
    fn mcp_parity_every_edit_command_variant_is_in_the_edit_schema() {
        let edit = catalog()
            .into_iter()
            .find(|tool| tool.name == "edit")
            .unwrap();
        let schema = &edit.input_schema["$defs"]["EditCommand"]["oneOf"];
        let in_schema: BTreeSet<String> = schema
            .as_array()
            .expect("EditCommand is a oneOf in the edit schema")
            .iter()
            .map(|variant| {
                variant["properties"]["command"]["const"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let variants: BTreeSet<String> = EditCommand::VARIANTS
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(
            variants, in_schema,
            "EditCommand variants and the edit tool's schema differ"
        );
    }

    // ---- the server over the real local transport (M-G14) ----------------------------------

    #[derive(Clone, Default)]
    struct Collect(Arc<Mutex<Vec<ModelPatch>>>);

    impl PatchSink for Collect {
        fn send(&self, patch: &ModelPatch) -> bool {
            self.0.lock().unwrap().push(patch.clone());
            true
        }
    }

    struct TestHost {
        ipc: IpcState,
        engine: EngineState,
        plugins: Plugins,
    }

    impl Host for TestHost {
        fn ipc(&self) -> &IpcState {
            &self.ipc
        }

        fn engine_state(&self) -> &EngineState {
            &self.engine
        }

        fn plugins(&self) -> &Plugins {
            &self.plugins
        }
    }

    /// A running listener over a test host, in a temporary data directory.
    struct Fixture {
        host: Arc<TestHost>,
        listener: McpListener,
        dir: PathBuf,
        work: tempfile::TempDir,
    }

    impl Fixture {
        fn start() -> Self {
            // Most tests never scan: the scanner child is only built for the ones that do.
            Self::start_with(ow_plugin_host::ScanCommand::new("ow-scan-not-built"))
        }

        fn start_with(scanner: ow_plugin_host::ScanCommand) -> Self {
            let work = tempfile::tempdir().unwrap();
            let cache =
                ow_plugin_host::PluginCache::new(Arc::new(ow_plugin_host::ThreadExecutor::new()));
            let host = Arc::new(TestHost {
                ipc: IpcState::with_plugins(cache.clone()),
                engine: EngineState::mock(work.path()),
                plugins: Plugins::new(cache, scanner, work.path().join("plugin-cache.json")),
            });
            let dir = endpoint::dir_in(&work.path().join("data"));
            let server = AppBackend::server(Arc::clone(&host) as Arc<dyn Host>);
            let listener = McpListener::start(&dir, server).unwrap();
            Self {
                host,
                listener,
                dir,
                work,
            }
        }

        async fn client(&self) -> Client {
            let stream = connect(&ClientConfig {
                dir: self.dir.clone(),
                wait: Duration::from_secs(10),
                launch: None,
            })
            .await
            .unwrap();
            Client::new(stream)
        }
    }

    struct Client {
        reader: BufReader<ReadHalf<Box<dyn AppStream>>>,
        writer: WriteHalf<Box<dyn AppStream>>,
        next: u64,
    }

    impl Client {
        fn new(stream: Box<dyn AppStream>) -> Self {
            let (read, writer) = tokio::io::split(stream);
            Self {
                reader: BufReader::new(read),
                writer,
                next: 1,
            }
        }

        async fn request(&mut self, method: &str, params: Value) -> Value {
            let id = self.next;
            self.next += 1;
            let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
            self.writer
                .write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
            let mut answer = String::new();
            self.reader.read_line(&mut answer).await.unwrap();
            let answer: Value = serde_json::from_str(&answer).unwrap();
            assert_eq!(answer["id"], id);
            answer
        }

        /// The structured result of a tools/call, asserting it succeeded.
        async fn call(&mut self, tool: &str, arguments: Value) -> Value {
            let answer = self
                .request("tools/call", json!({"name": tool, "arguments": arguments}))
                .await;
            let result = answer["result"].clone();
            assert_eq!(
                result["isError"], false,
                "{tool} failed: {}",
                result["content"][0]["text"]
            );
            result["structuredContent"].clone()
        }

        /// The error text of a tools/call that must fail.
        async fn call_error(&mut self, tool: &str, arguments: Value) -> String {
            let answer = self
                .request("tools/call", json!({"name": tool, "arguments": arguments}))
                .await;
            assert_eq!(
                answer["result"]["isError"], true,
                "{tool} should have failed"
            );
            answer["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn write_tone(path: &Path, amplitude: f64) {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..96_000_u32 {
            let sample = (amplitude * (TAU * 440.0 * f64::from(frame) / 48_000.0).sin()) as f32;
            writer.write_sample(sample).unwrap();
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn insert_track(id: u64) -> Value {
        serde_json::to_value(EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned::new(Track {
                id: TrackId(id),
                name: "Audio".into(),
                color: 0,
                kind: TrackKind::Audio,
                strip: Strip::default(),
                instrument: None,
                input: None,
                monitor: Default::default(),
            })),
        })
        .unwrap()
    }

    fn add_clip(id: u64, track: u64, media: u64) -> Value {
        serde_json::to_value(EditCommand::AddClip {
            clip: Box::new(Clip {
                id: id.into(),
                track: TrackId(track),
                name: "Tone".into(),
                start: 0.0,
                length: 4.0,
                muted: false,
                content: ClipContent::Audio {
                    media: MediaId(media),
                    source_offset: 0,
                    gain_db: 0.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                },
                take: None,
            }),
        })
        .unwrap()
    }

    #[test]
    fn mcp_lists_tools_reads_the_project_and_edits_reach_the_ui_mirror() {
        let fixture = Fixture::start();
        let host = &fixture.host;
        // The UI: a patch channel feeding a mirror, attached before the agent does anything.
        let sink = Collect::default();
        let mut mirror = Mirror::from_snapshot(host.ipc.attach(Box::new(sink.clone())));

        block_on(async {
            let mut client = fixture.client().await;
            let init = client
                .request(
                    "initialize",
                    json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
                )
                .await;
            assert_eq!(init["result"]["serverInfo"]["name"], "openwave-studio");
            let listed = client.request("tools/list", json!({})).await;
            let names: BTreeSet<String> = listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["name"].as_str().unwrap().to_owned())
                .collect();
            let wanted: BTreeSet<String> =
                TOOL_NAMES.iter().map(|name| (*name).to_owned()).collect();
            assert_eq!(names, wanted);

            // Read the project.
            let project = client.call("get_project", json!({})).await;
            assert_eq!(project["project"]["name"], "Untitled");
            assert_eq!(project["history"]["dirty"], false);

            // Reserve ids and edit with them: the command shapes are the schema's.
            let ids = client.call("alloc_ids", json!({"count": 2})).await["ids"]
                .as_array()
                .unwrap()
                .clone();
            let track = ids[0].as_u64().unwrap();
            let edited = client
                .call(
                    "edit",
                    json!({"commands": [insert_track(track), {"command": "set_name", "name": "From the agent"}], "label": "Agent edit"}),
                )
                .await;
            assert!(edited["ops_total"].as_u64().unwrap() >= 2);
            assert_eq!(edited["history"]["canUndo"], true);

            // The UI mirror received them as patches and equals the control state.
            for patch in sink.0.lock().unwrap().iter() {
                mirror.apply(patch).expect("patches arrive in order");
            }
            let snapshot = host.ipc.snapshot();
            assert_eq!(mirror.project, snapshot.project);
            assert_eq!(mirror.history, snapshot.history);
            assert_eq!(snapshot.project.name, "From the agent");
            assert_eq!(snapshot.project.tracks.len(), 1);
            assert!(snapshot.history.dirty);

            // Several commands are one undo step; the first refused one reverts them all.
            let refused = client
                .call_error(
                    "edit",
                    json!({"commands": [{"command": "set_name", "name": "Half done"}, {"command": "remove_track", "id": 9999}]}),
                )
                .await;
            assert!(refused.contains("command 2 of 2"), "{refused}");
            assert_eq!(host.ipc.snapshot().project.name, "From the agent");
            assert_eq!(host.ipc.snapshot().project.tracks.len(), 1);

            // Undo takes back the whole first edit at once; redo returns it.
            client.call("undo", json!({})).await;
            let after_undo = host.ipc.snapshot();
            assert_eq!(after_undo.project.name, "Untitled");
            assert!(after_undo.project.tracks.is_empty());
            client.call("redo", json!({})).await;
            assert_eq!(host.ipc.snapshot().project.tracks.len(), 1);
            assert!(client.call_error("redo", json!({})).await.len() > 3);

            // Arguments the schema refuses come back as an error result the model can fix.
            let bad = client.call_error("edit", json!({"commands": []})).await;
            assert!(
                bad.contains("at least one") || bad.contains("invalid"),
                "{bad}"
            );
            let unknown = client
                .request("tools/call", json!({"name": "nope", "arguments": {}}))
                .await;
            assert!(unknown["error"]["code"].is_i64());
        });
        // The UI saw every patch, including the refused edit's revert, the undo and the redo.
        let seen = mirror.seq as usize;
        for patch in sink.0.lock().unwrap().iter().skip(seen) {
            mirror.apply(patch).unwrap();
        }
        assert_eq!(mirror.project, host.ipc.snapshot().project);
    }

    #[test]
    fn mcp_imports_analyses_and_exports_with_overwrite_protection() {
        let fixture = Fixture::start();
        let wav = fixture.work.path().join("tone.wav");
        write_tone(&wav, 0.5);
        let out = fixture.work.path().join("mix.wav");
        block_on(async {
            let mut client = fixture.client().await;
            let imported = client
                .call("import_audio", json!({"path": wav.to_string_lossy()}))
                .await;
            let media = imported["media"].as_u64().unwrap();
            let ids = client.call("alloc_ids", json!({"count": 2})).await["ids"]
                .as_array()
                .unwrap()
                .clone();
            let (track, clip) = (ids[0].as_u64().unwrap(), ids[1].as_u64().unwrap());
            client
                .call(
                    "edit",
                    json!({"commands": [insert_track(track), add_clip(clip, track, media)]}),
                )
                .await;

            let report = client.call("analyze", json!({})).await;
            let lufs = report["integratedLufs"]
                .as_f64()
                .expect("a tone is not silent");
            assert!((-30.0..-3.0).contains(&lufs), "integrated loudness {lufs}");
            assert_eq!(report["bands"].as_array().unwrap().len(), 8);

            let exported = client
                .call(
                    "export_wav",
                    json!({"path": out.to_string_lossy(), "normalize": {"target_lufs": -14.0, "true_peak_ceiling_db": -1.0}}),
                )
                .await;
            let after = exported["analysis"]["integratedLufs"].as_f64().unwrap();
            assert!((after - -14.0).abs() < 0.6, "normalised to {after}");
            assert!(out.is_file());
            let first_size = std::fs::metadata(&out).unwrap().len();

            // Writing over the file needs overwrite=true.
            let refused = client
                .call_error("export_wav", json!({"path": out.to_string_lossy()}))
                .await;
            assert!(refused.contains("overwrite"), "{refused}");
            assert_eq!(std::fs::metadata(&out).unwrap().len(), first_size);
            client
                .call(
                    "export_wav",
                    json!({"path": out.to_string_lossy(), "overwrite": true}),
                )
                .await;
        });
    }

    #[test]
    fn mcp_project_files_need_explicit_discard_and_overwrite() {
        let fixture = Fixture::start();
        let bundle = fixture.work.path().join("song.owave");
        block_on(async {
            let mut client = fixture.client().await;
            client
                .call(
                    "edit",
                    json!({"commands": [{"command": "set_name", "name": "Song"}]}),
                )
                .await;
            // Unsaved changes are not thrown away by accident.
            let refused = client.call_error("project", json!({"action": "new"})).await;
            assert!(refused.contains("discard_changes"), "{refused}");
            assert_eq!(fixture.host.ipc.snapshot().project.name, "Song");

            let saved = client
                .call(
                    "project",
                    json!({"action": "save_as", "path": bundle.to_string_lossy()}),
                )
                .await;
            assert!(Path::new(saved["path"].as_str().unwrap()).is_dir());
            assert!(!fixture.host.ipc.snapshot().history.dirty);

            // Another project may not replace an existing bundle without overwrite=true.
            let other = fixture.work.path().join("other.owave");
            client.call("project", json!({"action": "new"})).await;
            client
                .call(
                    "project",
                    json!({"action": "save_as", "path": other.to_string_lossy()}),
                )
                .await;
            let refused = client
                .call_error(
                    "project",
                    json!({"action": "save_as", "path": bundle.to_string_lossy()}),
                )
                .await;
            assert!(refused.contains("overwrite"), "{refused}");

            // Open it again; it is clean, so nothing needs discarding.
            let opened = client
                .call(
                    "project",
                    json!({"action": "open", "path": bundle.to_string_lossy()}),
                )
                .await;
            assert_eq!(opened["name"], "Song");
        });
    }

    #[test]
    fn mcp_transport_audio_settings_and_the_plugin_editor_needs_a_hosted_plugin() {
        let fixture = Fixture::start();
        block_on(async {
            let mut client = fixture.client().await;
            let status = client.call("transport", json!({"action": "status"})).await;
            assert_eq!(status["engine"]["audioState"], "running");
            assert!(status["position_beats"].is_number());
            client
                .call("transport", json!({"action": "locate", "beats": 4}))
                .await;
            assert!(
                client
                    .call_error("transport", json!({"action": "locate"}))
                    .await
                    .contains("beats")
            );
            let devices = client
                .call("audio_settings", json!({"action": "list_devices"}))
                .await;
            assert!(devices["backends"].as_array().is_some());
            let kinds = client.call("get_device_types", json!({})).await;
            assert!(kinds["builtin"].as_array().unwrap().len() >= 5);

            // Preferences merge; the AI control switch is the user's alone.
            let set = client
                .call(
                    "audio_settings",
                    json!({"action": "set", "preferences": {"appearance": {"theme": "dark"}}}),
                )
                .await;
            assert_eq!(set["prefs"]["appearance"]["theme"], "dark");
            assert_eq!(set["prefs"]["ai"]["allowControl"], true);
            let refused = client
                .call_error(
                    "audio_settings",
                    json!({"action": "set", "preferences": {"ai": {"allowControl": false}}}),
                )
                .await;
            assert!(refused.contains("only be changed by the user"), "{refused}");

            // The editor belongs to a hosted plugin: a device that is not one is refused (and
            // Linux, which has no editor windows, says so first).
            if ow_plugin_host::editor::AVAILABLE {
                let refused = client
                    .call_error("plugin_editor", json!({"action": "open", "device": 1}))
                    .await;
                assert!(refused.contains("not a loaded plugin"), "{refused}");
            } else {
                let editor = client
                    .call("plugin_editor", json!({"action": "open", "device": 1}))
                    .await;
                assert_eq!(editor["available"], false);
                assert!(editor["reason"].as_str().unwrap().contains("Linux"));
            }
        });
    }

    /// The device ids of `insert_track` bundles that put the fixture gain plugin on an audio
    /// track.
    fn gain_track(track: u64, device: u64) -> Value {
        use ow_model::{Device, DeviceId};
        let mut bundle = Owned::new(Track {
            id: TrackId(track),
            name: "Audio".into(),
            color: 0,
            kind: TrackKind::Audio,
            strip: Strip {
                inserts: vec![DeviceId(device)],
                ..Strip::default()
            },
            instrument: None,
            input: None,
            monitor: Default::default(),
        });
        bundle.devices.push(Device {
            id: DeviceId(device),
            kind: "clap:com.openwave.fixture.gain".into(),
            name: "Gain".into(),
            enabled: true,
            params: Default::default(),
            state: vec![],
            sidechain: None,
        });
        serde_json::to_value(EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(bundle),
        })
        .unwrap()
    }

    #[test]
    fn mcp_scans_lists_and_hosts_plugins() {
        let fixture = Fixture::start_with(ow_plugin_host::ScanCommand::new(
            crate::plugins::testing::ow_scan(),
        ));
        let bundles = ow_plugin_fixtures::bundle_dir();
        block_on(async {
            let mut client = fixture.client().await;
            // Nothing is known before a scan; the fixtures folder is added as a scan folder the
            // way an agent does it, through the preferences.
            let before = client.call("list_plugins", json!({})).await;
            assert_eq!(before["plugins"].as_array().unwrap().len(), 0);
            client
                .call(
                    "audio_settings",
                    json!({"action": "set", "preferences": {"paths": {"pluginDirs": [bundles]}}}),
                )
                .await;

            // The scan loads each file in a child; the crashing fixture is reported, not fatal.
            let scan = client.call("scan_plugins", json!({})).await;
            let kinds: Vec<&str> = scan["plugins"]
                .as_array()
                .unwrap()
                .iter()
                .map(|plugin| plugin["kind"].as_str().unwrap())
                .collect();
            assert!(
                kinds.contains(&"clap:com.openwave.fixture.gain"),
                "{kinds:?}"
            );
            assert!(
                kinds.contains(&"clap:com.openwave.fixture.tone"),
                "{kinds:?}"
            );
            let failures = scan["failures"].as_array().unwrap();
            assert!(
                failures.iter().any(|failure| failure["status"] == "crashed"
                    && failure["path"]
                        .as_str()
                        .unwrap()
                        .ends_with("ow-fixture-crash.clap")),
                "{failures:?}"
            );
            let again = client.call("scan_plugins", json!({})).await;
            assert_eq!(again["scanned"], 0, "unchanged files come from the cache");
            let listed = client.call("list_plugins", json!({})).await;
            assert_eq!(listed["plugins"], again["plugins"]);

            // get_device_types lists them with their parameters.
            let types = client.call("get_device_types", json!({})).await;
            assert_eq!(types["plugins_available"], true);
            let gain = types["plugins"]
                .as_array()
                .unwrap()
                .iter()
                .find(|plugin| plugin["kind"] == "clap:com.openwave.fixture.gain")
                .expect("the gain plugin is listed");
            assert_eq!(gain["format"], "clap");
            assert_eq!(gain["instrument"], false);
            assert_eq!(gain["params"][1]["choices"], json!(["Normal", "Inverted"]));
            // The editor flag says whether this platform can show the plugin's editor.
            assert_eq!(
                gain["editor"] == json!(true),
                ow_plugin_host::editor::AVAILABLE
            );
            // VST3 classes are listed next to the CLAP plugins, with the controller's ranges.
            let vst3 = types["plugins"]
                .as_array()
                .unwrap()
                .iter()
                .find(|plugin| {
                    plugin["kind"] == format!("vst3:{}", ow_plugin_fixtures::VST3_GAIN_ID)
                })
                .expect("the VST3 gain plugin is listed");
            assert_eq!(vst3["format"], "vst3");
            assert_eq!(vst3["params"][0]["max"], 2.0);
            assert_eq!(vst3["params"][1]["choices"], json!(["Normal", "Inverted"]));

            // A plugin is a device like any other: added with an edit, its parameters set with
            // set_device_param, saved with the project.
            let ids = client.call("alloc_ids", json!({"count": 2})).await;
            let (track, device) = (
                ids["ids"][0].as_u64().unwrap(),
                ids["ids"][1].as_u64().unwrap(),
            );
            client
                .call("edit", json!({"commands": [gain_track(track, device)]}))
                .await;
            client
                .call(
                    "edit",
                    json!({"commands": [{"command": "set_device_param", "device": device, "param": 1, "value": 0.5}]}),
                )
                .await;
            let project = client.call("get_project", json!({})).await;
            let devices = project["project"]["devices"].as_array().unwrap();
            let hosted = devices
                .iter()
                .find(|d| d["id"] == device)
                .expect("the device exists");
            assert_eq!(hosted["params"]["1"], 0.5);

            // The plugin's editor opens and closes through the same tool (the test host's plugin
            // main thread is not the process's main thread, which AppKit insists on, so the
            // window itself is only created here on Windows).
            if cfg!(windows) {
                let opened = client
                    .call("plugin_editor", json!({"action": "open", "device": device}))
                    .await;
                assert_eq!(opened["open"], true);
                assert!(opened["width"].as_u64().unwrap() > 0);
                let plugin = fixture
                    .host
                    .plugins()
                    .cache()
                    .get(device)
                    .expect("the plugin is hosted");
                assert!(plugin.editor_open());
                let closed = client
                    .call(
                        "plugin_editor",
                        json!({"action": "close", "device": device}),
                    )
                    .await;
                assert_eq!(
                    (closed["open"].clone(), closed["was_open"].clone()),
                    (json!(false), json!(true))
                );
                assert!(!plugin.editor_open());
                let again = client
                    .call(
                        "plugin_editor",
                        json!({"action": "close", "device": device}),
                    )
                    .await;
                assert_eq!(again["was_open"], false);
            } else if !ow_plugin_host::editor::AVAILABLE {
                let editor = client
                    .call("plugin_editor", json!({"action": "open", "device": device}))
                    .await;
                assert_eq!(editor["available"], false);
            }
        });
        let live = fixture
            .host
            .ipc
            .with_control(|control| control.plugins().cloned());
        assert_eq!(live.expect("plugins are hosted").len(), 1);
    }

    #[test]
    fn mcp_refuses_connections_without_the_session_token() {
        let fixture = Fixture::start();
        block_on(async {
            // A wrong token is refused and the connection closed.
            let stream = open(fixture.listener.endpoint()).await.unwrap();
            let error = authenticate(stream, &"0".repeat(64)).await.err().unwrap();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);

            // Speaking MCP without presenting the token first gets no answer to the request.
            let mut stream = open(fixture.listener.endpoint()).await.unwrap();
            stream
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n")
                .await
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut answer = String::new();
            reader.read_line(&mut answer).await.unwrap();
            assert!(answer.starts_with("ERR"), "{answer}");
            answer.clear();
            assert_eq!(
                reader.read_line(&mut answer).await.unwrap(),
                0,
                "the connection is closed"
            );

            // The right token still works afterwards.
            let mut client = fixture.client().await;
            assert!(client.call("get_project", json!({})).await["project"].is_object());
        });
    }

    #[test]
    fn mcp_disabled_announcement_and_listener_lifecycle() {
        let fixture = Fixture::start();
        let state = McpState::default();
        let temp = tempfile::tempdir().unwrap();
        let dir = endpoint::dir_in(temp.path());
        let host = Arc::clone(&fixture.host);
        state.sync(true, &dir, || host as Arc<dyn Host>);
        let file = endpoint::read(&dir).unwrap();
        assert!(file.enabled && !file.token.is_empty());
        // Turning it off stops the listener and says so, without a token.
        state.sync(false, &dir, || {
            unreachable!("no host is needed to turn it off")
        });
        let file = endpoint::read(&dir).unwrap();
        assert!(!file.enabled && file.token.is_empty() && file.endpoint.is_empty());
        // The client refuses to start another app while this one runs with AI control off.
        let error = block_on(connect(&ClientConfig {
            dir: dir.clone(),
            wait: Duration::from_secs(2),
            launch: None,
        }))
        .err()
        .unwrap();
        assert_eq!(error, ow_mcp::client::ConnectError::Disabled);
        // Back on, with a fresh token.
        let host = Arc::clone(&fixture.host);
        state.sync(true, &dir, || host as Arc<dyn Host>);
        assert!(endpoint::read(&dir).unwrap().enabled);
        state.stop();
        assert!(endpoint::read(&dir).is_err());
    }

    fn device_json(id: u64) -> Value {
        json!({"id": id, "kind": "builtin.eq", "name": "EQ", "enabled": true,
               "params": {"0": 70.0, "3": -2.5}, "state": [], "sidechain": null})
    }

    #[test]
    fn mcp_edit_accepts_device_params_in_bundles_and_batches() {
        let fixture = Fixture::start();
        block_on(async {
            let mut client = fixture.client().await;
            let ids = client.call("alloc_ids", json!({"count": 3})).await["ids"]
                .as_array()
                .unwrap()
                .clone();
            let (track, device, nested) = (
                ids[0].as_u64().unwrap(),
                ids[1].as_u64().unwrap(),
                ids[2].as_u64().unwrap(),
            );
            let mut bundle = insert_track(track);
            bundle["bundle"]["devices"] = json!([device_json(device)]);
            bundle["bundle"]["item"]["strip"]["inserts"] = json!([device]);
            client.call("edit", json!({"commands": [bundle]})).await;
            let project = fixture.host.ipc.snapshot().project;
            assert_eq!(
                project.devices[&ow_model::DeviceId(device)].params[&0],
                70.0
            );
            assert_eq!(
                project.devices[&ow_model::DeviceId(device)].params[&3],
                -2.5
            );

            // The same through a batch nested inside edit.
            let insert = json!({"command": "insert_device",
                "slot": serde_json::to_value(ow_model::DeviceSlot::Insert {
                    strip: ow_model::MixerRef::Track(TrackId(track)),
                    index: 1,
                }).unwrap(),
                "bundle": {"device": device_json(nested), "lanes": []}});
            client
                .call(
                    "edit",
                    json!({"commands": [{"command": "batch", "commands": [insert]}]}),
                )
                .await;
            let project = fixture.host.ipc.snapshot().project;
            assert_eq!(
                project.devices[&ow_model::DeviceId(nested)].params[&0],
                70.0
            );
        });
    }

    #[test]
    fn mcp_transport_is_not_playing_after_a_locate_and_reports_the_target() {
        let fixture = Fixture::start();
        block_on(async {
            let mut client = fixture.client().await;
            client.call("transport", json!({"action": "stop"})).await;
            let located = client
                .call("transport", json!({"action": "locate", "beats": 8}))
                .await;
            let beats = located["position_beats"].as_f64().unwrap();
            assert!((beats - 8.0).abs() < 0.1, "locate answered {beats}");
            let status = client.call("transport", json!({"action": "status"})).await;
            assert_eq!(status["moving"], false);
        });
    }

    #[test]
    fn mcp_midi_tools_edit_notes_and_move_midi_files_with_guards() {
        let fixture = Fixture::start();
        block_on(async {
            let mut client = fixture.client().await;
            let ids = client.call("alloc_ids", json!({"count": 2})).await["ids"]
                .as_array()
                .unwrap()
                .clone();
            let (track, clip) = (ids[0].as_u64().unwrap(), ids[1].as_u64().unwrap());
            let mut bundle = insert_track(track);
            bundle["bundle"]["item"]["kind"] = json!("midi");
            bundle["bundle"]["clips"] = json!([{
                "id": clip, "track": track, "name": "Riff", "start": 0.0, "length": 4.0,
                "muted": false, "take": null,
                "content": {"type": "midi", "controllers": [], "notes": [
                    {"start": 0.2, "length": 0.5, "key": 60, "velocity": 100},
                    {"start": 1.0, "length": 0.5, "key": 127, "velocity": 100}]}
            }]);
            client.call("edit", json!({"commands": [bundle]})).await;

            // Note operations are computed by OpenWave: quantize one note, then transpose all.
            let reply = client
                .call(
                    "midi_edit",
                    json!({"clip": clip, "notes": [0], "edit": {"op": "quantize", "grid": 0.25}}),
                )
                .await;
            assert_eq!(
                (reply["notesBefore"].as_u64(), reply["notesAfter"].as_u64()),
                (Some(2), Some(2))
            );
            let notes = |ipc: &crate::ipc::IpcState| match &ipc.snapshot().project.clips
                [&ow_model::ClipId(clip)]
                .content
            {
                ow_model::ClipContent::Midi { notes, .. } => notes.clone(),
                _ => panic!("a MIDI clip"),
            };
            assert_eq!(notes(&fixture.host.ipc)[0].start, 0.25);
            let moved = client
                .call(
                    "midi_edit",
                    json!({"clip": clip, "edit": {"op": "transpose", "semitones": 1}}),
                )
                .await;
            assert_eq!(
                moved["notesAfter"], 1,
                "the note leaving the range is removed"
            );
            assert!(
                client
                    .call_error("midi_edit", json!({"clip": 9999, "edit": {"op": "legato"}}))
                    .await
                    .contains("no clip")
            );

            // Export and import: only .mid and .midi, never over an existing file by accident.
            let file = fixture.work.path().join("riff.mid");
            let written = client
                .call("export_midi", json!({"path": file.to_string_lossy()}))
                .await;
            assert_eq!(written["notes"], 1);
            assert!(file.is_file());
            let again = client
                .call_error("export_midi", json!({"path": file.to_string_lossy()}))
                .await;
            assert!(again.contains("overwrite"), "{again}");
            client
                .call(
                    "export_midi",
                    json!({"path": file.to_string_lossy(), "overwrite": true, "clips": [clip]}),
                )
                .await;
            let document = fixture.work.path().join("notes.docx");
            std::fs::write(&document, b"keep me").unwrap();
            for path in [&document, &fixture.work.path().join("riff.wav")] {
                let error = client
                    .call_error(
                        "export_midi",
                        json!({"path": path.to_string_lossy(), "overwrite": true}),
                    )
                    .await;
                assert!(error.contains(".mid"), "{error}");
            }
            assert_eq!(std::fs::read(&document).unwrap(), b"keep me");

            let imported = client
                .call(
                    "import_midi",
                    json!({"path": file.to_string_lossy(), "start_beat": 8}),
                )
                .await;
            assert_eq!(imported["clips"].as_array().unwrap().len(), 1);
            assert_eq!(imported["tracks"].as_array().unwrap().len(), 1);
            assert!(
                client
                    .call_error("import_midi", json!({"path": document.to_string_lossy()}))
                    .await
                    .contains(".mid")
            );
        });
    }

    #[test]
    fn mcp_export_and_save_never_replace_other_kinds_of_file() {
        let fixture = Fixture::start();
        let document = fixture.work.path().join("notes.docx");
        std::fs::write(&document, b"precious").unwrap();
        let folder = fixture.work.path().join("photos.owave");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("keep.txt"), b"keep").unwrap();
        block_on(async {
            let mut client = fixture.client().await;
            for path in [
                document.clone(),
                fixture.work.path().join("mix.mp3"),
                fixture.work.path().join("mix"),
            ] {
                let error = client
                    .call_error(
                        "export_wav",
                        json!({"path": path.to_string_lossy(), "overwrite": true}),
                    )
                    .await;
                assert!(error.contains(".wav"), "{error}");
            }
            assert_eq!(std::fs::read(&document).unwrap(), b"precious");
            let upper = fixture.work.path().join("LOUD.WAV");
            client
                .call("export_wav", json!({"path": upper.to_string_lossy()}))
                .await;
            assert!(upper.is_file());

            // An existing folder that is not a project bundle is refused, overwrite or not.
            let error = client
                .call_error(
                    "project",
                    json!({"action": "save_as", "path": folder.to_string_lossy(), "overwrite": true}),
                )
                .await;
            assert!(error.contains("not an OpenWave project bundle"), "{error}");
            assert!(!folder.join(ow_model::PROJECT_FILE).exists());
            // A file that merely lacks the extension is not touched: `.owave` is appended.
            client
                .call(
                    "project",
                    json!({"action": "save_as", "path": document.to_string_lossy()}),
                )
                .await;
            assert_eq!(std::fs::read(&document).unwrap(), b"precious");
        });
    }
}
