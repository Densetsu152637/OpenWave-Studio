//! OpenWave Studio application crate: windows, commands and data paths.

pub mod cli;
pub mod engine;
pub mod ipc;
pub mod paths;
pub mod portable_update;
pub mod prefs;
pub mod translocation;
pub mod update;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

use crate::engine::EngineState;
use crate::paths::{DefaultDirs, Paths};

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
            app.manage(ipc::IpcState::new());
            app.manage(paths);
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
            ipc::project_new,
            ipc::project_open,
            ipc::project_save,
            ipc::media_import,
            ipc::export_wav,
            ipc::record::record_set_arm,
            ipc::record::record_start,
            ipc::telemetry_start,
            ipc::telemetry_set_strips,
            ipc::telemetry_stop,
            ipc::peaks_range,
            update::update_status,
            update::update_check,
            update::update_set_channel,
            update::update_restart
        ])
        .build(tauri::generate_context!());
    match result {
        // A downloaded update installs as the app exits.
        Ok(app) => app.run(|handle, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                update::install_on_exit(handle);
            }
        }),
        Err(error) => {
            eprintln!("[openwave] fatal: {error}");
            std::process::exit(1);
        }
    }
}
