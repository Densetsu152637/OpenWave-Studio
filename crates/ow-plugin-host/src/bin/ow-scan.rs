//! `ow-scan <plugin file>`: loads one plugin file, lists its plugins and prints the result.
//!
//! The scanner starts this program once per candidate file so that a plugin that crashes or
//! hangs while loading costs only this process. The app's `--scan-plugin` flag does the same
//! thing through [`ow_plugin_host::scan::run_child`].

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: ow-scan <plugin file>");
        return ExitCode::from(64);
    };
    let code = ow_plugin_host::scan::run_child(&path);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
