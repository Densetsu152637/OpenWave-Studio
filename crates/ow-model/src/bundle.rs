//! The `.owave` project bundle on disk: atomic saves and rotating autosaves.
//!
//! ```text
//! Name.owave/
//!   project.json   the model with "format_version"
//!   audio/         imported and recorded media
//!   cache/         peaks and stretch caches (safe to delete)
//!   backups/       rotating autosaves: project-YYYYMMDD-HHMMSS.json (UTC)
//! ```
//!
//! # Atomic save
//!
//! [`write_atomic`] writes the new content to `project.json.tmp` in the same directory, flushes it
//! to disk (`File::sync_all`: `fsync` on Unix, `FlushFileBuffers` on Windows) and only then renames
//! it over `project.json`. A crash or error before the rename leaves the previous `project.json`
//! untouched (a stale `.tmp` is overwritten by the next save and ignored on load); after the
//! rename the file is the complete new one.
//!
//! * Unix: `rename(2)` over an existing file is atomic, and the parent directory is `fsync`ed
//!   afterwards so the rename itself survives power loss.
//! * Windows: `std::fs::rename` replaces an existing file (`MoveFileExW` with
//!   `MOVEFILE_REPLACE_EXISTING`, or the POSIX-semantics rename where the file system offers it).
//!   Antivirus and indexers may briefly hold the destination open and make the rename fail with
//!   a sharing violation, so it is retried a few times. NTFS journals the rename; flushing a
//!   directory handle needs `FILE_FLAG_BACKUP_SEMANTICS`, which `std` does not expose, so the
//!   directory flush is skipped there.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::format::{self, FORMAT_VERSION, FormatError, MAX_PROJECT_BYTES};
use crate::model::Project;

/// Name of the model file inside a bundle.
pub const PROJECT_FILE: &str = "project.json";
/// Subdirectories a bundle always has.
pub const BUNDLE_DIRS: [&str; 3] = ["audio", "cache", "backups"];
/// Autosaves kept by default.
pub const DEFAULT_BACKUP_LIMIT: usize = 20;

/// Why a bundle operation failed.
#[derive(Debug)]
pub enum BundleError {
    /// A file system error.
    Io(io::Error),
    /// The project file is invalid.
    Format(FormatError),
    /// A zip archive error.
    Zip(zip::result::ZipError),
    /// The archive holds something that must not be extracted, or is not a project.
    Unsafe(String),
    /// The destination exists and is not an empty directory.
    DestinationExists(PathBuf),
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "file error: {e}"),
            Self::Format(e) => e.fmt(f),
            Self::Zip(e) => write!(f, "archive error: {e}"),
            Self::Unsafe(why) => write!(f, "rejected archive: {why}"),
            Self::DestinationExists(path) => {
                write!(f, "{} already exists and is not empty", path.display())
            }
        }
    }
}

impl std::error::Error for BundleError {}

impl From<io::Error> for BundleError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FormatError> for BundleError {
    fn from(error: FormatError) -> Self {
        Self::Format(error)
    }
}

impl From<zip::result::ZipError> for BundleError {
    fn from(error: zip::result::ZipError) -> Self {
        Self::Zip(error)
    }
}

/// Points of [`write_atomic_with`] a fault can be injected at (tests simulate a crash there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicStage {
    /// The temporary file is written but not yet flushed to disk.
    TempWritten,
    /// The temporary file is flushed; the rename has not happened.
    TempSynced,
}

/// Writes `data` to `path` atomically: the old content or the new, never a mix.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    write_atomic_with(path, data, &mut |_| Ok(()))
}

/// Like [`write_atomic`], calling `hook` at each [`AtomicStage`]; an `Err` from it aborts the
/// write there (fault injection for tests). The previous content of `path` is untouched on
/// every error, and the temporary file is removed.
pub fn write_atomic_with(
    path: &Path,
    data: &[u8],
    hook: &mut dyn FnMut(AtomicStage) -> io::Result<()>,
) -> io::Result<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let temp = temp_path(path, file_name);
    let result = write_then_rename(path, &temp, data, hook);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// A temporary name unique to this process and call (`name.<pid>.<n>.tmp`), so concurrent
/// writers and leftovers of a crashed run never collide.
pub(crate) fn temp_path(path: &Path, file_name: &std::ffi::OsStr) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut name = file_name.to_os_string();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}

/// Removes leftover `<file>.tmp` and `<file>.<pid>.<n>.tmp` files of crashed writes.
fn remove_stale_temps(dir: &Path, file: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && name.starts_with(file)
            && name.ends_with(".tmp")
            && name[file.len()..].starts_with('.')
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn write_then_rename(
    path: &Path,
    temp: &Path,
    data: &[u8],
    hook: &mut dyn FnMut(AtomicStage) -> io::Result<()>,
) -> io::Result<()> {
    let mut file = File::create(temp)?;
    file.write_all(data)?;
    hook(AtomicStage::TempWritten)?;
    file.sync_all()?;
    drop(file);
    hook(AtomicStage::TempSynced)?;
    rename_replacing(temp, path)?;
    sync_parent(path);
    Ok(())
}

/// Renames over an existing file, retrying briefly on sharing violations (Windows scanners).
pub(crate) fn rename_replacing(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied && attempt < 5 => {
                attempt += 1;
                thread::sleep(Duration::from_millis(20 * attempt));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
pub(crate) fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        })
    {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
pub(crate) fn sync_parent(_path: &Path) {}

/// Creates the bundle directory and its subdirectories if missing.
pub fn create_dirs(dir: &Path) -> io::Result<()> {
    for sub in BUNDLE_DIRS {
        fs::create_dir_all(dir.join(sub))?;
    }
    Ok(())
}

/// Saves `project` into the bundle `dir` (created if needed), atomically replacing
/// `project.json`. The project is validated first, so a saved file always loads.
pub fn save(dir: &Path, project: &Project) -> Result<(), BundleError> {
    let bytes = format::to_json(project)?;
    create_dirs(dir)?;
    preserve_old_version(dir, FORMAT_VERSION)?;
    remove_stale_temps(dir, PROJECT_FILE);
    write_atomic(&dir.join(PROJECT_FILE), &bytes)?;
    Ok(())
}

/// If the bundle's `project.json` is from a version older than `current`, copies it to
/// `backups/pre-migration-v<N>-<UTC stamp>.json` (not an autosave name, so rotation never
/// deletes it) and returns the copy's path. [`save`] calls this before the first write over an
/// older file; an error aborts the save so the old file is never replaced unpreserved.
pub fn preserve_old_version(dir: &Path, current: u32) -> io::Result<Option<PathBuf>> {
    let file = File::open(dir.join(PROJECT_FILE));
    let mut bytes = Vec::new();
    match file {
        Ok(file) => {
            file.take(MAX_PROJECT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let Some(version) = format::peek_version(&bytes).filter(|v| *v < u64::from(current)) else {
        return Ok(None);
    };
    let backups = dir.join("backups");
    fs::create_dir_all(&backups)?;
    let mut seconds = now_seconds();
    let path = loop {
        let candidate = backups.join(format!(
            "pre-migration-v{version}-{}.json",
            utc_stamp(seconds)
        ));
        if !candidate.exists() {
            break candidate;
        }
        seconds += 1;
    };
    write_atomic(&path, &bytes)?;
    Ok(Some(path))
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Loads the project from the bundle `dir`.
pub fn load(dir: &Path) -> Result<Project, BundleError> {
    load_file(&dir.join(PROJECT_FILE))
}

/// Loads a project from a JSON file (a bundle's `project.json` or a backup).
pub fn load_file(path: &Path) -> Result<Project, BundleError> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_PROJECT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(format::from_json(&bytes)?)
}

/// Writes a rotating autosave of `project` into `dir/backups/` as
/// `project-YYYYMMDD-HHMMSS.json` (UTC time of `at`; moved forward a second at a time if that
/// name exists), then deletes the oldest backups beyond `keep` (at least 1). Returns the
/// path written. Only files matching that name pattern are ever deleted.
pub fn autosave(
    dir: &Path,
    project: &Project,
    at: SystemTime,
    keep: usize,
) -> Result<PathBuf, BundleError> {
    let bytes = format::to_json(project)?;
    let backups = dir.join("backups");
    fs::create_dir_all(&backups)?;
    let mut seconds = at
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = loop {
        let candidate = backups.join(format!("project-{}.json", utc_stamp(seconds)));
        if !candidate.exists() {
            break candidate;
        }
        seconds += 1;
    };
    write_atomic(&path, &bytes)?;
    // Rotation is best effort: the autosave itself succeeded, and the file just written is
    // never a candidate for deletion.
    let _ = prune_keeping(dir, keep, Some(&path));
    Ok(path)
}

/// The autosaves in `dir/backups/`, newest first.
pub fn list_backups(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut names: Vec<String> = Vec::new();
    let entries = match fs::read_dir(dir.join("backups")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str()
            && is_backup_name(name)
            && entry.file_type()?.is_file()
        {
            names.push(name.to_owned());
        }
    }
    names.sort_unstable_by(|a, b| b.cmp(a));
    Ok(names
        .into_iter()
        .map(|n| dir.join("backups").join(n))
        .collect())
}

/// Deletes all but the newest `keep` (at least 1) autosaves.
pub fn prune_backups(dir: &Path, keep: usize) -> io::Result<()> {
    prune_keeping(dir, keep, None)
}

/// Like [`prune_backups`], but `protect` (the backup just written) always stays and counts
/// toward `keep`, so even a file stamped older than `keep` existing backups survives and the
/// total stays at `keep`. Keeps going after a failed delete and reports the first error.
fn prune_keeping(dir: &Path, keep: usize, protect: Option<&Path>) -> io::Result<()> {
    let keep = keep.max(1);
    let mut room = keep - usize::from(protect.is_some());
    let mut first_error = None;
    for old in list_backups(dir)? {
        if Some(old.as_path()) == protect {
            continue;
        }
        if room > 0 {
            room -= 1;
            continue;
        }
        if let Err(error) = fs::remove_file(&old) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// `project-YYYYMMDD-HHMMSS.json`.
fn is_backup_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 28
        && name.starts_with("project-")
        && name.ends_with(".json")
        && bytes[8..16].iter().all(u8::is_ascii_digit)
        && bytes[16] == b'-'
        && bytes[17..23].iter().all(u8::is_ascii_digit)
}

/// `YYYYMMDD-HHMMSS` for a Unix time in UTC.
fn utc_stamp(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_calendar_dates() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(951_782_400), "20000229-000000");
        assert_eq!(utc_stamp(1_785_586_245), "20260801-121045");
        assert_eq!(utc_stamp(4_102_444_799), "20991231-235959");
    }

    #[test]
    fn backup_names_match_exactly() {
        assert!(is_backup_name("project-20260101-120000.json"));
        assert!(!is_backup_name("project-20260101-120000.json.tmp"));
        assert!(!is_backup_name("project-2026010-1120000.json"));
        assert!(!is_backup_name("notes.json"));
    }
}
