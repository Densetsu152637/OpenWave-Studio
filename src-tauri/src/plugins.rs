//! Third-party plugin scanning and the registry the engine, the UI and the MCP tools share
//! (slice M8).
//!
//! `ow-plugin-host` does the work; this module is the app's wiring of it:
//!
//! * the [`MainThreadExecutor`] the plugins' main-thread calls run on: Tauri's event-loop
//!   thread on Windows and macOS ([`TauriExecutor`], `AppHandle::run_on_main_thread`), a
//!   dedicated thread on Linux (architecture section 4);
//! * [`Plugins`]: the plugin cache (instances, the directory of scanned plugins), the scanner
//!   (the app re-launches its own executable as `openwave-studio --scan-plugin <file>` for each
//!   candidate, 10 s timeout) and the `plugin-cache.json` scan cache in the app data folder;
//! * the conversion of scanned plugins to the [`DeviceDescriptor`]s the UI and MCP show.
//!
//! Scan folders are the OS defaults, `CLAP_PATH` and `VST3_PATH`, and the user's extra folders
//! from the preferences (`paths.pluginDirs`).
//!
//! Editor windows (slice M9): `plugin_editor_open` and `plugin_editor_close` (and the MCP
//! `plugin_editor` tool, which calls the same [`Plugins`] methods) show and hide a hosted
//! plugin's own editor. A parameter changed in an editor reaches the project as
//! `SetDeviceParam` edits, one undo step per gesture ([`start_background`] starts the thread
//! that applies them): the plugin reports on the plugin main thread, which must not wait for
//! the control lock, so the reports are handed to that thread through a channel.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::engine::EngineState;
use crate::ipc::IpcState;
use crate::ipc::control::PlanTarget;

use ow_ipc::{
    DeviceDescriptor, EditCommand, ParamDescriptorDto, ParamScale, ParamUnit, PluginScanFailure,
    PluginScanSummary, UiCommand,
};
use ow_model::DeviceId;
use ow_plugin_host::scan::default_paths;
use ow_plugin_host::{
    EditPhase, EditorEdit, FileRecord, MainThreadExecutor, PluginCache, PluginInfo, ScanCommand,
    ScanStatus, Scanner,
};
#[cfg(not(target_os = "linux"))]
use ow_plugin_host::{ExecutorError, executor::Job};

/// File name of the scan cache in the plugin cache folder.
pub const CACHE_FILE: &str = "plugin-cache.json";

/// Name of the event the UI listens to: the list of plugins changed.
pub const PLUGINS_CHANGED: &str = "plugins-changed";

/// Runs plugin main-thread work on Tauri's event-loop thread (Windows and macOS: AppKit and
/// Win32 windows have to live there).
#[cfg(not(target_os = "linux"))]
pub struct TauriExecutor {
    app: tauri::AppHandle,
    main: std::thread::ThreadId,
}

#[cfg(not(target_os = "linux"))]
impl TauriExecutor {
    /// An executor for `app`. Call it on the main thread (the setup hook is): that thread is
    /// what [`MainThreadExecutor::is_main_thread`] answers for.
    pub fn new(app: tauri::AppHandle) -> Self {
        Self {
            app,
            main: std::thread::current().id(),
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl MainThreadExecutor for TauriExecutor {
    fn dispatch(&self, job: Job) -> Result<(), ExecutorError> {
        self.app
            .run_on_main_thread(job)
            .map_err(|_| ExecutorError::Gone)
    }

    fn is_main_thread(&self) -> bool {
        std::thread::current().id() == self.main
    }
}

/// The executor for this platform in the running app.
pub fn app_executor(app: &tauri::AppHandle) -> Arc<dyn MainThreadExecutor> {
    #[cfg(not(target_os = "linux"))]
    {
        Arc::new(TauriExecutor::new(app.clone()))
    }
    #[cfg(target_os = "linux")]
    {
        let _ = app;
        Arc::new(ow_plugin_host::ThreadExecutor::new())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the last scan found.
#[derive(Default)]
struct Last {
    files: Vec<FileRecord>,
    scanned: usize,
    folders: Vec<PathBuf>,
}

/// The plugin registry of the running app.
pub struct Plugins {
    cache: PluginCache,
    scanner: Scanner,
    last: Mutex<Last>,
    /// Held while a scan runs, so two scans never overlap.
    scanning: Mutex<()>,
    /// Set when the app is closing: the background thread ends.
    stop: AtomicBool,
}

impl Plugins {
    /// A registry over `cache` that scans with `command` and keeps results in `cache_file`.
    pub fn new(cache: PluginCache, command: ScanCommand, cache_file: PathBuf) -> Self {
        Self {
            cache,
            scanner: Scanner::new(command).with_cache(cache_file),
            last: Mutex::new(Last::default()),
            scanning: Mutex::new(()),
            stop: AtomicBool::new(false),
        }
    }

    /// A registry that scans with this executable (`--scan-plugin`), the app's normal setup.
    pub fn for_app(cache: PluginCache, cache_dir: &std::path::Path) -> Self {
        let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("openwave-studio"));
        Self::new(
            cache,
            ScanCommand::with_args(program, &["--scan-plugin"]),
            cache_dir.join(CACHE_FILE),
        )
    }

    /// The plugin cache the engine compiles against.
    pub fn cache(&self) -> &PluginCache {
        &self.cache
    }

    /// Fills the directory from the scan cache without starting any child (what the app does
    /// first, so plugins are usable at once).
    pub fn load_cached(&self) {
        let cache = self.scanner.load_cache();
        let mut last = lock(&self.last);
        last.files = cache.files;
        let plugins = plugins_of(&last.files);
        self.cache.directory().set(plugins);
    }

    /// Scans the default folders, `CLAP_PATH` and `extra` (absolute folders from the
    /// preferences), reusing the cache for files that did not change unless `force`. Updates
    /// the directory and answers what is usable and what had to be given up on.
    pub fn scan(&self, extra: &[String], force: bool) -> PluginScanSummary {
        let _scanning = lock(&self.scanning);
        let mut folders = default_paths();
        folders.extend(
            extra
                .iter()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
        );
        folders.dedup();
        let report = self.scanner.scan(&folders, force);
        self.cache.directory().set(report.plugins());
        *lock(&self.last) = Last {
            files: report.files,
            scanned: report.scanned,
            folders,
        };
        self.summary()
    }

    /// What the directory holds and which files failed, from the last scan (or the cache).
    pub fn summary(&self) -> PluginScanSummary {
        let last = lock(&self.last);
        PluginScanSummary {
            plugins: self.descriptors(),
            failures: last.files.iter().filter_map(failure_of).collect(),
            files: u32::try_from(last.files.len()).unwrap_or(u32::MAX),
            scanned: u32::try_from(last.scanned).unwrap_or(u32::MAX),
            folders: last
                .folders
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        }
    }

    /// Opens the editor window of the hosted plugin on device `device` (or brings it forward) and
    /// answers its client size. Fails with a reason when the device is not a loaded plugin, the
    /// plugin has no editor or this platform shows none.
    pub fn open_editor(&self, device: u64) -> Result<(u32, u32), String> {
        let plugin = self
            .cache
            .get(device)
            .ok_or_else(|| format!("device {device} is not a loaded plugin"))?;
        plugin.open_editor().map_err(|error| error.to_string())?;
        Ok(plugin.editor_size().unwrap_or((0, 0)))
    }

    /// Closes the editor window of the plugin on device `device`. Returns whether an editor was
    /// open. Fails when the device is not a loaded plugin.
    pub fn close_editor(&self, device: u64) -> Result<bool, String> {
        let plugin = self
            .cache
            .get(device)
            .ok_or_else(|| format!("device {device} is not a loaded plugin"))?;
        let was_open = plugin.editor_open();
        plugin.close_editor();
        Ok(was_open)
    }

    /// The usable plugins as device descriptors.
    pub fn descriptors(&self) -> Vec<DeviceDescriptor> {
        self.cache
            .directory()
            .all()
            .iter()
            .map(descriptor_of)
            .collect()
    }
}

fn plugins_of(files: &[FileRecord]) -> Vec<PluginInfo> {
    files
        .iter()
        .filter(|file| file.status.is_ok())
        .flat_map(|file| file.plugins.iter().cloned())
        .collect()
}

fn failure_of(file: &FileRecord) -> Option<PluginScanFailure> {
    let (status, detail) = match &file.status {
        ScanStatus::Ok => return None,
        ScanStatus::Crashed { detail } => ("crashed", detail.clone()),
        ScanStatus::TimedOut => (
            "timed_out",
            "the scanner did not finish in 10 seconds".into(),
        ),
        ScanStatus::Failed { message } => ("failed", message.clone()),
    };
    Some(PluginScanFailure {
        path: file.path.clone(),
        status: status.into(),
        detail,
    })
}

/// A scanned plugin as the UI and the MCP tools describe it.
pub fn descriptor_of(info: &PluginInfo) -> DeviceDescriptor {
    DeviceDescriptor {
        kind: info.kind.clone(),
        name: info.name.clone(),
        instrument: info.instrument,
        params: info
            .params
            .iter()
            .filter(|param| !param.hidden)
            .map(|param| ParamDescriptorDto {
                id: param.id,
                name: if param.module.is_empty() {
                    param.name.clone()
                } else {
                    format!("{} / {}", param.module, param.name)
                },
                min: param.min,
                max: param.max,
                default: param.default,
                unit: ParamUnit::None,
                scale: ParamScale::Linear,
                choices: param.choices.clone(),
            })
            .collect(),
        format: Some(info.format.prefix().to_string()),
        vendor: Some(info.vendor.clone()),
        editor: (info.editor && ow_plugin_host::editor::AVAILABLE).then_some(true),
    }
}

/// Built-in and plugin device kinds together, built-ins first.
pub fn all_descriptors(plugins: Option<&Plugins>) -> Vec<DeviceDescriptor> {
    let mut all = ow_ipc::device_descriptors();
    if let Some(plugins) = plugins {
        all.extend(plugins.descriptors());
    }
    all
}

/// Scans the plugin folders, updates the plugin list the UI and the tools see and recompiles
/// a project that uses plugins (one may have become available).
fn scan_and_refresh(app: &AppHandle, force: bool) -> PluginScanSummary {
    let plugins = app.state::<Plugins>();
    let extra = app
        .state::<EngineState>()
        .preferences()
        .paths
        .plugin_dirs
        .clone();
    let summary = plugins.scan(&extra, force);
    app.state::<IpcState>()
        .refresh_plugins(app.state::<EngineState>().engine());
    let _ = app.emit(PLUGINS_CHANGED, ());
    summary
}

/// Scans the standard plugin folders and the user's extra folders (preferences, Settings ›
/// Folders). Each plugin file is loaded in a child process, so a plugin that crashes or hangs
/// is skipped; files that did not change since the last scan are not loaded again unless
/// `force` is set. Answers the plugins that can be used and the files that cannot.
#[tauri::command]
pub async fn plugins_scan(
    app: AppHandle,
    force: Option<bool>,
) -> Result<PluginScanSummary, String> {
    tauri::async_runtime::spawn_blocking(move || scan_and_refresh(&app, force.unwrap_or(false)))
        .await
        .map_err(|error| error.to_string())
}

/// What the plugin scanner knows: usable plugins, failed files, folders searched. Scans
/// nothing.
#[tauri::command]
pub async fn plugins_list(app: AppHandle) -> Result<PluginScanSummary, String> {
    Ok(app.state::<Plugins>().summary())
}

/// Shows the editor window of the plugin on device `device`, or brings it forward.
#[tauri::command]
pub async fn plugin_editor_open(app: AppHandle, device: u64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<Plugins>().open_editor(device).map(|_| ())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Closes the editor window of the plugin on device `device` (nothing happens when it is not
/// open).
#[tauri::command]
pub async fn plugin_editor_close(app: AppHandle, device: u64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<Plugins>().close_editor(device).map(|_| ())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Applies one parameter change reported by a plugin's editor to the project. A gesture is a
/// transaction (one undo step), so `Begin` and `End` open and close one; a change outside any
/// gesture is a step of its own. Refusals (the device was removed meanwhile) are dropped.
pub(crate) fn apply_editor_edit(
    ipc: &IpcState,
    target: &dyn PlanTarget,
    plugins: &PluginCache,
    device: u64,
    edit: EditorEdit,
    open: &mut HashSet<(u64, u32)>,
) {
    let key = (device, edit.param);
    let command = match edit.phase {
        EditPhase::Begin => {
            let name = plugins
                .get(device)
                .and_then(|plugin| plugin.info().param(edit.param).map(|p| p.name.clone()))
                .unwrap_or_else(|| "plugin parameter".into());
            open.insert(key);
            UiCommand::Begin {
                label: format!("Edit {name}"),
            }
        }
        EditPhase::Change => UiCommand::Edit {
            edit: EditCommand::SetDeviceParam {
                device: DeviceId(device),
                param: edit.param,
                value: Some(edit.value),
            },
        },
        EditPhase::End => {
            if !open.remove(&key) {
                return;
            }
            UiCommand::End
        }
    };
    let begun = matches!(command, UiCommand::Begin { .. });
    let result = ipc.with_control(|control| control.command(command, target));
    if result.is_err() && begun {
        open.remove(&key);
    }
}

/// Starts the background work of the plugin registry: one scan at startup (the cache answers
/// for files that did not change), then a watch over what plugins ask of the host (a latency
/// change needs a recompile so delay compensation follows it, an open editor needs its window
/// events served), and the thread that applies parameter changes made in plugin editors.
pub fn start_background(app: &AppHandle) {
    let (sender, receiver) = mpsc::channel::<(u64, EditorEdit)>();
    app.state::<Plugins>()
        .cache()
        .set_edit_sink(Arc::new(move |device, edit| {
            let _ = sender.send((device, edit));
        }));
    let editing = app.clone();
    let spawned = std::thread::Builder::new()
        .name("plugin-edits".into())
        .spawn(move || {
            let mut open = HashSet::new();
            while let Ok((device, edit)) = receiver.recv() {
                let ipc = editing.state::<IpcState>();
                let engine = editing.state::<EngineState>();
                let plugins = editing.state::<Plugins>();
                apply_editor_edit(
                    &ipc,
                    engine.engine(),
                    plugins.cache(),
                    device,
                    edit,
                    &mut open,
                );
            }
        });
    if let Err(error) = spawned {
        eprintln!("[openwave] could not start the plugin edit thread: {error}");
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("plugin-registry".into())
        .spawn(move || {
            scan_and_refresh(&app, false);
            while !app.state::<Plugins>().stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(100));
                let requests = app.state::<Plugins>().cache().service();
                if requests.iter().any(|(_, request)| request.latency_changed) {
                    app.state::<IpcState>()
                        .refresh_plugins(app.state::<EngineState>().engine());
                }
            }
        });
    if let Err(error) = spawned {
        eprintln!("[openwave] could not start the plugin registry thread: {error}");
    }
}

/// Stops the background thread and releases every plugin instance (the app is closing).
pub fn shutdown(app: &AppHandle) {
    app.state::<Plugins>().stop.store(true, Ordering::Release);
    app.state::<IpcState>().release_plugins();
}

/// Helpers for tests that need the scanner child.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::OnceLock;

    /// The `ow-scan` binary, built on demand so a fresh checkout passes: `cargo build` of the
    /// binary reports where the executable went.
    pub(crate) fn ow_scan() -> PathBuf {
        static PATH: OnceLock<PathBuf> = OnceLock::new();
        PATH.get_or_init(|| {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let output = Command::new(cargo)
                .args([
                    "build",
                    "-p",
                    "ow-plugin-host",
                    "--bin",
                    "ow-scan",
                    "--message-format=json-render-diagnostics",
                ])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .expect("run cargo to build ow-scan");
            assert!(output.status.success(), "building ow-scan failed");
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|message| message["reason"] == "compiler-artifact")
                .filter(|message| message["target"]["name"] == "ow-scan")
                .find_map(|message| message["executable"].as_str().map(PathBuf::from))
                .expect("cargo reported the ow-scan executable")
        })
        .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ow_plugin_host::ThreadExecutor;

    #[test]
    fn a_scanned_plugin_becomes_a_descriptor_with_its_choices() {
        let plugins = ow_plugin_host::scan::scan_in_process(&ow_plugin_fixtures::gain_path())
            .expect("the gain fixture scans");
        let gain = plugins
            .iter()
            .find(|p| p.id == ow_plugin_fixtures::GAIN_ID)
            .unwrap();
        let descriptor = descriptor_of(gain);
        assert_eq!(descriptor.kind, "clap:com.openwave.fixture.gain");
        assert_eq!(descriptor.format.as_deref(), Some("clap"));
        assert!(!descriptor.instrument);
        let polarity = descriptor
            .params
            .iter()
            .find(|p| p.id == ow_plugin_fixtures::PARAM_POLARITY)
            .unwrap();
        assert_eq!(polarity.choices, ["Normal", "Inverted"]);
    }

    #[test]
    fn a_registry_without_a_working_scanner_reports_every_file_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("x.clap");
        std::fs::write(&fake, b"x").unwrap();
        let cache = PluginCache::new(Arc::new(ThreadExecutor::new()));
        let plugins = Plugins::new(
            cache,
            ScanCommand::new("this-scanner-does-not-exist"),
            dir.path().join(CACHE_FILE),
        );
        let summary = plugins.scan(&[dir.path().to_string_lossy().into_owned()], false);
        assert!(summary.plugins.is_empty());
        assert_eq!(summary.failures.len(), 1);
        assert_eq!(summary.failures[0].status, "failed");
        // A relative extra folder is ignored.
        let summary = plugins.scan(&["relative/dir".into()], false);
        assert!(!summary.folders.iter().any(|f| f.contains("relative")));
    }
}
