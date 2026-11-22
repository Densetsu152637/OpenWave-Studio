//! Command line flags handled in `main()` before Tauri initializes.

use std::ffi::OsString;
use std::io::Write;

/// The build's effective version: CI injects `OW_VERSION` at build time so it matches the
/// channel version passed to `tauri build --config`; local builds fall back to the crate
/// version (which `tauri.conf.json` mirrors).
pub fn effective_version() -> &'static str {
    option_env!("OW_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// Exactly what `--version` writes to stdout: the bare version and a newline.
pub fn version_output(version: &str) -> String {
    format!("{version}\n")
}

/// Handles flags that must not start the app. Returns the process exit code when the
/// invocation was fully handled, or `None` to continue starting the app.
pub fn handle_args<I>(args: I) -> Option<i32>
where
    I: IntoIterator<Item = OsString>,
{
    let wants_version = args.into_iter().any(|arg| arg == "--version");
    if !wants_version {
        return None;
    }
    write_stdout(&version_output(effective_version()));
    Some(0)
}

#[cfg(not(windows))]
fn write_stdout(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// Release builds on Windows use the `windows` subsystem, so the process has no console of
/// its own. Attach to the parent's console first; then write to stdout if the parent
/// redirected it (a pipe or file), otherwise to the console itself.
#[cfg(windows)]
#[allow(unsafe_code)]
fn write_stdout(text: &str) {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
    };

    // SAFETY: plain Win32 calls with constant arguments; a failed attach (no parent console)
    // is expected and ignored.
    let redirected = unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        !handle.is_null() && handle != INVALID_HANDLE_VALUE
    };
    if redirected {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    } else if let Ok(mut console) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
        let _ = console.write_all(text.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_output_is_the_bare_version_and_a_newline() {
        assert_eq!(version_output("0.1.0"), "0.1.0\n");
        assert_eq!(
            version_output("1.2.34-experimental"),
            "1.2.34-experimental\n"
        );
    }

    #[test]
    fn crate_version_matches_tauri_conf() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        assert_eq!(conf["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn only_the_version_flag_is_handled() {
        assert_eq!(handle_args(Vec::<OsString>::new()), None);
        assert_eq!(handle_args([OsString::from("--other")]), None);
    }
}
