//! macOS App Translocation detection.
//!
//! The builds are not notarized, so every `.app` downloaded through a browser carries the
//! quarantine attribute. Gatekeeper then runs a quarantined app that was launched from where it
//! was unzipped from a randomized read-only copy under `.../AppTranslocation/...`. The `portable`
//! marker beside the real bundle is invisible from there and the updater cannot replace the
//! bundle, so the app tells the user to move it (with its marker) to another folder or to clear
//! the quarantine attribute, and keeps portable mode and updates off for that run.

use std::path::{Component, Path};

/// Directory name Gatekeeper uses for translocated copies.
const TRANSLOCATION_DIR: &str = "AppTranslocation";

/// Whether `exe` runs from a translocated copy: any path component is `AppTranslocation`.
pub fn is_translocated(exe: &Path) -> bool {
    exe.components()
        .any(|part| matches!(part, Component::Normal(name) if name == TRANSLOCATION_DIR))
}

/// Whether the running executable is translocated (always false off macOS).
pub fn is_translocated_exe() -> bool {
    cfg!(target_os = "macos") && std::env::current_exe().is_ok_and(|exe| is_translocated(&exe))
}

/// What the prompt tells the user.
pub const PROMPT_TITLE: &str = "Move OpenWave Studio before using it";
pub const PROMPT_TEXT: &str = "macOS started OpenWave Studio from a temporary, read-only copy because it was downloaded with a browser and has not been moved. In this state portable mode and updates are switched off.\n\nQuit, move OpenWave Studio.app (together with its \"portable\" file, if you use portable mode) to another folder such as Applications, and open it from there. Or run this in Terminal and open it again:\n\nxattr -dr com.apple.quarantine \"/path/to/OpenWave Studio.app\"";

/// Builds the AppleScript for a blocking dialog with a default "Quit" button.
#[cfg(any(target_os = "macos", test))]
fn dialog_script() -> String {
    let escape = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "display dialog \"{}\" with title \"{}\" buttons {{\"Continue anyway\", \"Quit\"}} default button \"Quit\" with icon caution",
        escape(PROMPT_TEXT),
        escape(PROMPT_TITLE)
    )
}

/// Shows the blocking move prompt when the app is translocated, before the main window opens.
/// Returns `false` when the user chose to quit (the caller exits); `true` to continue, which is
/// also the answer when the app is not translocated or the dialog cannot be shown.
#[cfg(target_os = "macos")]
pub fn prompt_if_translocated() -> bool {
    if !is_translocated_exe() {
        return true;
    }
    eprintln!("[openwave] translocated: portable mode and updates are off until the app is moved");
    match std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(dialog_script())
        .output()
    {
        Ok(output) if output.status.success() => {
            !String::from_utf8_lossy(&output.stdout).contains("button returned:Quit")
        }
        // Closing the dialog (Escape) counts as quitting; any other failure to show it must not
        // lock the user out.
        Ok(output) => !String::from_utf8_lossy(&output.stderr).contains("User canceled"),
        Err(error) => {
            eprintln!("[openwave] translocation prompt failed: {error}");
            true
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn prompt_if_translocated() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translocation_detects_a_gatekeeper_randomized_path() {
        let exe = Path::new(
            "/private/var/folders/zz/abc123/T/AppTranslocation/1B2C3D4E-0000-1111-2222-333344445555/d/OpenWave Studio.app/Contents/MacOS/OpenWave Studio",
        );
        assert!(is_translocated(exe));
    }

    #[test]
    fn translocation_ignores_normal_locations() {
        for path in [
            "/Applications/OpenWave Studio.app/Contents/MacOS/OpenWave Studio",
            "/Users/me/Downloads/OpenWave Studio.app/Contents/MacOS/OpenWave Studio",
            "/Users/me/AppTranslocationNotes/OpenWave Studio.app/Contents/MacOS/x",
            "C:\\Apps\\OpenWave Studio.exe",
        ] {
            assert!(!is_translocated(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn translocation_prompt_names_both_ways_out_and_builds_a_valid_script() {
        assert!(PROMPT_TEXT.contains("xattr -dr com.apple.quarantine"));
        assert!(PROMPT_TEXT.contains("move OpenWave Studio.app"));
        let script = dialog_script();
        assert!(script.starts_with("display dialog \""));
        assert!(script.contains("default button \"Quit\""));
        // Quotes inside the text are escaped, so the script stays one string literal.
        assert!(script.contains("\\\"portable\\\""));
    }

    #[test]
    fn translocation_is_never_reported_off_macos() {
        if !cfg!(target_os = "macos") {
            assert!(!is_translocated_exe());
        }
    }
}
