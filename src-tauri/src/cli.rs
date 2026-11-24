//! Command line flags handled in `main()` before Tauri initializes.

use std::ffi::OsString;
use std::io::Write;

/// The build's effective version: CI injects `OW_VERSION` at build time so it matches the
/// channel version passed to `tauri build --config`; local builds fall back to the crate
/// version (which `tauri.conf.json` mirrors). An empty `OW_VERSION` counts as unset.
pub fn effective_version() -> &'static str {
    pick_version(option_env!("OW_VERSION"), env!("CARGO_PKG_VERSION"))
}

fn pick_version<'a>(injected: Option<&'a str>, fallback: &'a str) -> &'a str {
    injected
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(fallback)
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
    let args: Vec<OsString> = args.into_iter().collect();
    if args.iter().any(|arg| arg == "--version") {
        write_stdout(&version_output(effective_version()));
        return Some(0);
    }
    if is_mcp(&args) {
        return Some(run_mcp());
    }
    if let Some(path) = scan_plugin_path(&args) {
        return Some(ow_plugin_host::scan::run_child(std::path::Path::new(&path)));
    }
    None
}

/// The file of `openwave-studio --scan-plugin <file>`: the plugin scanner's child mode. The app
/// re-launches itself this way for every candidate plugin file, so a plugin that crashes or
/// hangs while it is loaded takes down only that process (architecture section 8). The process
/// prints one result line and exits; no window is created.
fn scan_plugin_path(args: &[OsString]) -> Option<OsString> {
    match args {
        [flag, path, ..] if flag == "--scan-plugin" => Some(path.clone()),
        _ => None,
    }
}

/// True for `openwave-studio mcp`: the first argument is the subcommand.
fn is_mcp(args: &[OsString]) -> bool {
    args.first().is_some_and(|arg| arg == "mcp")
}

/// `openwave-studio mcp`: an MCP server on stdio that relays to the running app's local socket,
/// starting the app (a copy of this executable, detached) when none is running and waiting up to
/// 20 seconds for its socket. Protocol messages go to stdout, everything else to stderr.
fn run_mcp() -> i32 {
    let Some(data_dir) = crate::paths::data_dir_without_tauri() else {
        eprintln!("openwave-studio mcp: could not find the app data directory");
        return 1;
    };
    let launch: ow_mcp::client::Launcher = Box::new(|| {
        let exe = std::env::current_exe()?;
        ow_mcp::client::launch_detached(&exe)
    });
    ow_mcp::client::run_stdio(ow_mcp::client::ClientConfig {
        dir: ow_mcp::endpoint::dir_in(&data_dir),
        wait: ow_mcp::client::LAUNCH_WAIT,
        launch: Some(launch),
    })
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
    fn empty_injected_version_falls_back() {
        assert_eq!(pick_version(Some("1.2.3"), "0.1.0"), "1.2.3");
        assert_eq!(pick_version(Some(""), "0.1.0"), "0.1.0");
        assert_eq!(pick_version(Some("  "), "0.1.0"), "0.1.0");
        assert_eq!(pick_version(None, "0.1.0"), "0.1.0");
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

    #[test]
    fn mcp_is_the_first_argument_only() {
        assert!(is_mcp(&[OsString::from("mcp")]));
        assert!(!is_mcp(&[]));
        assert!(!is_mcp(&[OsString::from("--other"), OsString::from("mcp")]));
        assert!(!is_mcp(&[OsString::from("mcpx")]));
    }

    #[test]
    fn scan_plugin_takes_the_file_after_the_flag() {
        let args = |parts: &[&str]| parts.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            scan_plugin_path(&args(&["--scan-plugin", "a.clap"])),
            Some(OsString::from("a.clap"))
        );
        assert_eq!(scan_plugin_path(&args(&["--scan-plugin"])), None);
        assert_eq!(scan_plugin_path(&args(&["a.clap"])), None);
        assert_eq!(
            scan_plugin_path(&args(&["mcp", "--scan-plugin", "a.clap"])),
            None
        );
    }
}
