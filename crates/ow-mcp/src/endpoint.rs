//! Where the app announces its MCP endpoint, and how that announcement is protected.
//!
//! While the listener runs, the app writes `<data dir>/mcp/mcp.json`:
//!
//! ```json
//! {"version":1,"enabled":true,"pid":1234,"endpoint":"\\\\.\\pipe\\openwave-studio-mcp-me-9f3a...","token":"<64 hex>"}
//! ```
//!
//! The directory and the file are readable by the current user only (see `secure`), so reading
//! the token is what proves the caller is the user. `endpoint` is the Windows named pipe (with a
//! random suffix chosen per run, so nobody can pre-create the name) or the Unix socket path. When
//! the user turned AI control off, the file says `"enabled":false` and carries no endpoint or
//! token, so `openwave-studio mcp` does not start a second copy of the app.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::secure;

/// Folder inside the app data directory.
pub const DIR_NAME: &str = "mcp";
/// The announcement file inside it.
pub const FILE_NAME: &str = "mcp.json";
/// Current file format.
pub const FORMAT_VERSION: u32 = 1;

/// Longest Unix socket path that fits `sockaddr_un` on every platform we run on.
#[cfg(unix)]
const MAX_SOCKET_PATH: usize = 100;

/// What the file says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointFile {
    /// Format version.
    pub version: u32,
    /// False while AI control is off in the settings.
    pub enabled: bool,
    /// Process id of the app that wrote the file (to tell a running app from a stale file).
    pub pid: u32,
    /// Named pipe name or socket path; empty when disabled.
    pub endpoint: String,
    /// Hex session token; empty when disabled.
    pub token: String,
}

/// The `mcp` directory under an app data directory.
pub fn dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join(DIR_NAME)
}

/// A fresh random 256-bit token as 64 hex characters.
pub fn new_token() -> io::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// A new endpoint name for this run: a named pipe on Windows (per-user, random suffix), the
/// socket file inside `dir` elsewhere.
pub fn new_endpoint(dir: &Path) -> io::Result<String> {
    #[cfg(windows)]
    {
        let _ = dir;
        let user: String = std::env::var("USERNAME")
            .unwrap_or_default()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            .take(32)
            .collect();
        let user = if user.is_empty() { "user".into() } else { user };
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(format!(
            r"\\.\pipe\openwave-studio-mcp-{user}-{}",
            hex(&random)
        ))
    }
    #[cfg(unix)]
    {
        let path = dir.join("openwave.sock");
        if path.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(io::Error::other(format!(
                "the socket path {} is too long for a Unix socket",
                path.display()
            )));
        }
        Ok(path.to_string_lossy().into_owned())
    }
}

/// Creates the private directory (if needed) and writes the file atomically with private
/// permissions.
pub fn write(dir: &Path, file: &EndpointFile) -> io::Result<()> {
    secure::create_private_dir(dir)?;
    let path = dir.join(FILE_NAME);
    let temp = dir.join(format!("{FILE_NAME}.tmp"));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options.open(&temp)?;
        secure::restrict_file(&temp)?;
        let text = serde_json::to_string(file).map_err(io::Error::other)?;
        handle.write_all(text.as_bytes())?;
        handle.sync_all()?;
        drop(handle);
        fs::rename(&temp, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Reads the file.
pub fn read(dir: &Path) -> io::Result<EndpointFile> {
    let text = fs::read_to_string(dir.join(FILE_NAME))?;
    serde_json::from_str(&text).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Deletes the file and (on Unix) the socket. Missing files are fine.
pub fn remove(dir: &Path) {
    let _ = fs::remove_file(dir.join(FILE_NAME));
    #[cfg(unix)]
    let _ = fs::remove_file(dir.join("openwave.sock"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_256_bits_of_hex_and_differ() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn the_file_round_trips_and_can_be_removed() {
        let temp = tempfile::tempdir().unwrap();
        let dir = dir_in(temp.path());
        let file = EndpointFile {
            version: FORMAT_VERSION,
            enabled: true,
            pid: 7,
            endpoint: new_endpoint(&dir).unwrap(),
            token: new_token().unwrap(),
        };
        write(&dir, &file).unwrap();
        assert_eq!(read(&dir).unwrap(), file);
        remove(&dir);
        assert!(read(&dir).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let dir = dir_in(temp.path());
        let file = EndpointFile {
            version: FORMAT_VERSION,
            enabled: false,
            pid: 1,
            endpoint: String::new(),
            token: String::new(),
        };
        write(&dir, &file).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(FILE_NAME)), 0o600);
    }
}
