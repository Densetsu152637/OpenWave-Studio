//! OpenWave Studio application crate: windows, commands and data paths.

pub mod cli;
pub mod engine;
pub mod ipc;
pub mod mcp;
pub mod paths;
pub mod plugins;
pub mod portable_update;
pub mod prefs;
pub mod translocation;
pub mod update;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

use crate::engine::EngineState;
use crate::paths::{DefaultDirs, Paths};

/// Every command registered with `generate_handler!` below, by name. The `mcp_parity` tests
/// check this list against the registration and against the MCP tool routes
/// ([`mcp::ROUTES`]), so a command cannot be added without an MCP tool (or a reasoned
/// exclusion in the test).
pub const COMMANDS: &[&str] = &[
    "engine_status",
    "set_test_tone",
    "transport_play",
    "transport_stop",
    "transport_locate",
    "prefs_get",
    "prefs_set",
    "audio_devices",
    "audio_retry",
    "ipc_attach",
    "ipc_snapshot",
    "ipc_command",
    "ipc_alloc_ids",
    "device_descriptors",
    "plugins_scan",
    "plugins_list",
    "plugin_editor_open",
    "plugin_editor_close",
    "project_new",
    "project_open",
    "project_save",
    "media_import",
    "library_scan",
    "library_scan_status",
    "library_scan_cancel",
    "library_search",
    "library_preview",
    "library_preview_stop",
    "export_wav",
    "export_mix",
    "analyze",
    "record_set_arm",
    "record_start",
    "midi_edit",
    "midi_import",
    "midi_export",
    "telemetry_start",
    "telemetry_set_strips",
    "telemetry_stop",
    "peaks_range",
    "update_status",
    "update_check",
    "update_set_channel",
    "update_restart",
    "update_install_now",
];

/// Starts the application. Never returns until the last window closes.
pub fn run() {
    // A quarantined, translocated macOS app gets a blocking prompt before anything else starts.
    if !translocation::prompt_if_translocated() {
        return;
    }
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let resolver = app.path();
            let data = resolver.app_data_dir()?;
            let paths = Paths::detect(DefaultDirs {
                documents: resolver.document_dir().unwrap_or_else(|_| data.clone()),
                data,
                config: resolver.app_config_dir()?,
                cache: resolver.app_cache_dir()?,
                logs: resolver.app_log_dir()?,
            });
            if paths.portable {
                eprintln!(
                    "[openwave] portable mode: data in {}",
                    paths.data_dir.display()
                );
                paths.ensure_dirs()?;
            }

            let mut window = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("OpenWave Studio")
                .inner_size(1280.0, 800.0)
                .min_inner_size(900.0, 560.0);
            if let Some(dir) = &paths.webview_data_dir {
                window = window.data_directory(dir.clone());
            }
            window.build()?;

            app.manage(EngineState::start(&paths));
            // Plugins run their main-thread work on the executor of this platform; the scan
            // cache gives the plugin list back at once, a background scan refreshes it.
            let cache = ow_plugin_host::PluginCache::new(plugins::app_executor(app.handle()));
            let registry = plugins::Plugins::for_app(cache.clone(), &paths.plugin_cache_dir);
            registry.load_cached();
            app.manage(ipc::IpcState::with_plugins(cache));
            app.manage(registry);
            app.manage(paths);
            plugins::start_background(app.handle());
            app.manage(mcp::McpState::default());
            mcp::sync_with_preferences(app.handle());
            update::setup(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine::engine_status,
            engine::set_test_tone,
            engine::transport_play,
            engine::transport_stop,
            engine::transport_locate,
            engine::prefs_get,
            engine::prefs_set,
            engine::audio_devices,
            engine::audio_retry,
            ipc::ipc_attach,
            ipc::ipc_snapshot,
            ipc::ipc_command,
            ipc::ipc_alloc_ids,
            ipc::device_descriptors,
            plugins::plugins_scan,
            plugins::plugins_list,
            plugins::plugin_editor_open,
            plugins::plugin_editor_close,
            ipc::project_new,
            ipc::project_open,
            ipc::project_save,
            ipc::media_import,
            ipc::library_scan,
            ipc::library_scan_status,
            ipc::library_scan_cancel,
            ipc::library_search,
            ipc::library_preview,
            ipc::library_preview_stop,
            ipc::export_wav,
            ipc::export_mix,
            ipc::analyze,
            ipc::record::record_set_arm,
            ipc::record::record_start,
            ipc::midi::midi_edit,
            ipc::midi::midi_import,
            ipc::midi::midi_export,
            ipc::telemetry_start,
            ipc::telemetry_set_strips,
            ipc::telemetry_stop,
            ipc::peaks_range,
            update::update_status,
            update::update_check,
            update::update_set_channel,
            update::update_restart,
            update::update_install_now
        ])
        .build(tauri::generate_context!());
    match result {
        // A downloaded update installs as the app exits.
        Ok(app) => app.run(|handle, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                mcp::shutdown(handle);
                plugins::shutdown(handle);
                update::install_on_exit(handle);
            }
        }),
        Err(error) => {
            eprintln!("[openwave] fatal: {error}");
            std::process::exit(1);
        }
    }
}
