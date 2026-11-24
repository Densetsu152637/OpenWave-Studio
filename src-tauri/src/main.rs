// Release builds use the `windows` subsystem (no console window); `cli` re-attaches the
// parent console when a command line flag needs to print.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Handled before Tauri initializes so headless smoke tests never open a window.
    if let Some(code) = openwave_studio_lib::cli::handle_args(std::env::args_os().skip(1)) {
        std::process::exit(code);
    }
    // The portable update helper (a copy of this exe run with `--apply-update`) never opens a window.
    if let Some(code) =
        openwave_studio_lib::portable_update::handle_args(std::env::args_os().skip(1))
    {
        std::process::exit(code);
    }
    openwave_studio_lib::run();
}
