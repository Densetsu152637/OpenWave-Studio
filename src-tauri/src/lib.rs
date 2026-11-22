//! OpenWave Studio application crate: windows, commands and data paths.

pub mod cli;
pub mod engine;
pub mod paths;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

use crate::engine::EngineState;
use crate::paths::{DefaultDirs, Paths};

/// Starts the application. Never returns until the last window closes.
pub fn run() {
    let result = tauri::Builder::default()
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

            app.manage(paths);
            app.manage(EngineState::start());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine::engine_status,
            engine::set_test_tone
        ])
        .run(tauri::generate_context!());
    if let Err(error) = result {
        eprintln!("[openwave] fatal: {error}");
        std::process::exit(1);
    }
}
