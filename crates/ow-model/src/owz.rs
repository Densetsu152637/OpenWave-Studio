//! `.owz` archives: a zip of a bundle for sharing and templates.
//!
//! An archive holds `project.json` and the `audio/` tree (not `cache/` or `backups/`), with `/`
//! separators and no top-level folder. Opening one extracts it to a new bundle directory.
//! Archives come from other people, so extraction only ever writes `project.json` and safe
//! paths under `audio/` (see [`is_safe_media_path`]), refuses symlinks and entries that could
//! escape the destination, bounds sizes and counts, and validates the project before returning.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::bundle::{
    BundleError, PROJECT_FILE, create_dirs, load, rename_replacing, sync_parent, temp_path,
};
use crate::format::MAX_PROJECT_BYTES;
use crate::model::Project;
use crate::validate::is_safe_media_path;

/// Most entries an archive may hold.
pub const MAX_ENTRIES: usize = 200_000;
/// Most bytes one archive entry may unpack to: 4 GiB.
pub const MAX_ENTRY_BYTES: u64 = 4 << 30;
/// Most bytes an archive may unpack to in total (sum of declared sizes): 32 GiB.
pub const MAX_UNPACKED_BYTES: u64 = 32 << 30;

/// Extraction limits; [`import`] uses the defaults above.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Most entries.
    pub max_entries: usize,
    /// Most bytes per entry.
    pub max_entry_bytes: u64,
    /// Most bytes in total.
    pub max_total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: MAX_ENTRIES,
            max_entry_bytes: MAX_ENTRY_BYTES,
            max_total_bytes: MAX_UNPACKED_BYTES,
        }
    }
}

/// Writes the bundle `dir` as an archive at `archive`, replacing any file there atomically
/// (the archive is written next to it and renamed). The project is loaded and validated first.
pub fn export(dir: &Path, archive: &Path) -> Result<(), BundleError> {
    let _ = load(dir)?;
    let file_name = archive
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "archive has no file name"))?;
    let temp = temp_path(archive, file_name);
    let result = write_archive(dir, &temp);
    match result {
        Ok(()) => rename_replacing(&temp, archive)
            .map(|()| sync_parent(archive))
            .map_err(|e| {
                let _ = fs::remove_file(&temp);
                BundleError::from(e)
            }),
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    }
}

fn write_archive(dir: &Path, temp: &Path) -> Result<(), BundleError> {
    let file = File::create(temp)?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    add_file(&mut zip, &dir.join(PROJECT_FILE), PROJECT_FILE)?;
    let audio = dir.join("audio");
    if audio.is_dir() {
        add_tree(&mut zip, &audio, "audio")?;
    }
    let mut writer = zip.finish()?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

fn add_tree<W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    dir: &Path,
    prefix: &str,
) -> Result<(), BundleError> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let entry_name = format!("{prefix}/{name}");
        // Symlinks are skipped: a bundle never needs them and they could leak other files.
        let kind = entry.file_type()?;
        if kind.is_dir() {
            add_tree(zip, &entry.path(), &entry_name)?;
        } else if kind.is_file() && is_safe_media_path(&entry_name) {
            add_file(zip, &entry.path(), &entry_name)?;
        }
    }
    Ok(())
}

fn add_file<W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    path: &Path,
    name: &str,
) -> Result<(), BundleError> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(size >= u64::from(u32::MAX));
    zip.start_file(name, options)?;
    io::copy(&mut file, zip)?;
    Ok(())
}

/// Extracts the archive into the new bundle directory `dest` (which must not exist or must be
/// empty), validates the project and returns it. On any failure `dest` is left as it was found
/// (removed when this call created it).
pub fn import(archive: &Path, dest: &Path) -> Result<Project, BundleError> {
    import_with_limits(archive, dest, Limits::default())
}

/// [`import`] with explicit [`Limits`].
pub fn import_with_limits(
    archive: &Path,
    dest: &Path,
    limits: Limits,
) -> Result<Project, BundleError> {
    let existed = dest.exists();
    if existed && (!dest.is_dir() || fs::read_dir(dest)?.next().is_some()) {
        return Err(BundleError::DestinationExists(dest.to_path_buf()));
    }
    fs::create_dir_all(dest)?;
    match extract(archive, dest, limits) {
        Ok(project) => Ok(project),
        Err(error) => {
            if existed {
                clear_dir(dest);
            } else {
                let _ = fs::remove_dir_all(dest);
            }
            Err(error)
        }
    }
}

fn clear_dir(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path: PathBuf = entry.path();
            let _ = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
        }
    }
}

fn unsafe_entry(name: &str, why: &str) -> BundleError {
    BundleError::Unsafe(format!("{name:?}: {why}"))
}

fn extract(archive: &Path, dest: &Path, limits: Limits) -> Result<Project, BundleError> {
    let mut zip = ZipArchive::new(BufReader::new(File::open(archive)?))?;
    if zip.len() > limits.max_entries {
        return Err(BundleError::Unsafe("too many entries".into()));
    }
    create_dirs(dest)?;
    let mut total: u64 = 0;
    let mut has_project = false;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let name = entry.name()?.into_owned();
        if entry.is_symlink() {
            return Err(unsafe_entry(&name, "symbolic link"));
        }
        if name.starts_with("cache/") || name.starts_with("backups/") {
            continue;
        }
        if entry.size() > limits.max_entry_bytes {
            return Err(unsafe_entry(&name, "entry unpacks to too much data"));
        }
        total = total.saturating_add(entry.size());
        if total > limits.max_total_bytes {
            return Err(BundleError::Unsafe("unpacks to too much data".into()));
        }
        if entry.is_dir() {
            let dir_name = name.trim_end_matches('/');
            if dir_name == "audio" {
                continue;
            }
            if !is_safe_media_path(dir_name) {
                return Err(unsafe_entry(&name, "unexpected directory"));
            }
            fs::create_dir_all(dest.join(dir_name))?;
            continue;
        }
        let is_project = name == PROJECT_FILE;
        if is_project {
            if entry.size() > MAX_PROJECT_BYTES as u64 {
                return Err(unsafe_entry(&name, "project file too large"));
            }
            has_project = true;
        } else if !is_safe_media_path(&name) {
            return Err(unsafe_entry(&name, "unexpected file"));
        }
        let target = dest.join(&name);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // `create_new` refuses to overwrite or follow anything already there.
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        let size = entry.size();
        copy_exact(&mut entry, &mut out, size)
            .map_err(|e| BundleError::Unsafe(format!("{name:?}: {e}")))?;
        out.sync_all()?;
    }
    if !has_project {
        return Err(BundleError::Unsafe(
            "the archive has no project.json".into(),
        ));
    }
    load(dest)
}

/// Copies exactly `size` bytes (the declared size) and fails on a short or long stream.
fn copy_exact(reader: &mut impl Read, writer: &mut impl Write, size: u64) -> io::Result<()> {
    let copied = io::copy(&mut reader.take(size), writer)?;
    if copied != size {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "entry is shorter than declared",
        ));
    }
    let mut extra = [0_u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(io::Error::other("entry is longer than declared"));
    }
    Ok(())
}
