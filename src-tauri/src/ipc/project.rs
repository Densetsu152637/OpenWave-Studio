//! Project files: new, open and save (the `project_*` commands, as plain functions so they can
//! be tested without a window).
//!
//! A project is an `.owave` bundle directory (`project.json`, `audio/`, `cache/`, `backups/`;
//! see `ow_model::bundle`). A project that was never saved keeps its imported media in a
//! temporary folder the control owns; saving copies (never moves) that media into the bundle's
//! `audio/`, points the control at the bundle and only then deletes the temporary folder.
//! Peak files (`cache/<hash>.owpk`) are rebuilt in the background when missing and every media
//! item is registered with the peak store whenever a project is opened or saved.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ow_ipc::{Project, Snapshot};
use ow_media::peaks::spawn_build;
use ow_model::bundle;

use super::IpcState;
use super::control::PlanTarget;

/// Extension of a project bundle.
pub const BUNDLE_EXTENSION: &str = "owave";

/// `path` as an absolute bundle path with `.owave` appended unless that is already its
/// extension (compared ignoring case): `song.v2` becomes `song.v2.owave`.
pub fn bundle_path(path: &Path) -> PathBuf {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(BUNDLE_EXTENSION))
    {
        path
    } else {
        let mut name = path.into_os_string();
        name.push(".");
        name.push(BUNDLE_EXTENSION);
        PathBuf::from(name)
    }
}

/// Copies `from` to `to` through a temporary file in the same folder, so a partial copy never
/// carries the real name. Does nothing when `to` already has the same length (media files are
/// named by content hash).
fn copy_media(from: &Path, to: &Path) -> io::Result<()> {
    let length = fs::metadata(from)?.len();
    if fs::metadata(to).is_ok_and(|existing| existing.len() == length) {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temp = to.as_os_str().to_owned();
    temp.push(".copying");
    let temp = PathBuf::from(temp);
    let result = fs::copy(from, &temp).and_then(|_| fs::rename(&temp, to));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Where the peak file of a media item with content hash `hash` lives under `root`.
pub(super) fn peak_path(root: &Path, hash: &str) -> PathBuf {
    root.join("cache").join(format!("{hash}.owpk"))
}

impl IpcState {
    /// Starts an empty, unsaved project (the UI receives a `Reset` patch). The previous
    /// project's temporary media folder is deleted.
    pub fn project_new(&self, target: &dyn PlanTarget) -> Result<Snapshot, String> {
        let mut control = super::lock(&self.control);
        let old_temp = control.temp.take();
        control.ctx.media_root = PathBuf::new();
        control.ctx.sources.clear();
        control.replace_project(Project::new("Untitled"), None, target);
        drop(old_temp);
        Ok(control.snapshot())
    }

    /// Opens the bundle at `path`. Nothing changes when it cannot be loaded.
    pub fn project_open(&self, path: &Path, target: &dyn PlanTarget) -> Result<Snapshot, String> {
        let dir = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let project = bundle::load(&dir)
            .map_err(|error| format!("could not open {}: {error}", dir.display()))?;
        let mut control = super::lock(&self.control);
        let old_temp = control.temp.take();
        control.ctx.media_root = dir.clone();
        control.ctx.sources.retain_under(&dir);
        self.prepare_peaks(&dir, &project);
        control.replace_project(project, Some(dir.to_string_lossy().into_owned()), target);
        drop(old_temp);
        Ok(control.snapshot())
    }

    /// Saves the project to the bundle `path` (`.owave` is added when it has no extension) or,
    /// without a path, to the bundle it came from; returns the bundle's path. Saving to a new
    /// place copies the media over and moves the project there: later imports and plans use the
    /// bundle, and the temporary media folder of an unsaved project is deleted. The dirty flag
    /// clears.
    pub fn project_save(
        &self,
        path: Option<&Path>,
        target: &dyn PlanTarget,
    ) -> Result<String, String> {
        let mut control = super::lock(&self.control);
        let dest = match path {
            Some(path) => bundle_path(path),
            None => control
                .session()
                .project_path()
                .map(PathBuf::from)
                .ok_or("the project has not been saved yet: choose where to save it")?,
        };
        let old_root = control.ctx.media_root.clone();
        let project = control.session().project().clone();
        let moving = control.session().project_path().map(Path::new) != Some(dest.as_path());
        bundle::create_dirs(&dest)
            .map_err(|error| format!("could not create {}: {error}", dest.display()))?;
        if moving {
            // Media that is already missing stays missing; any other failure stops the save
            // before the project file is written.
            for item in project.media_pool.values() {
                match copy_media(&old_root.join(&item.path), &dest.join(&item.path)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!("could not copy {}: {error}", item.path));
                    }
                }
            }
        }
        bundle::save(&dest, &project)
            .map_err(|error| format!("could not save {}: {error}", dest.display()))?;

        let saved_path = dest.to_string_lossy().into_owned();
        if moving {
            let old_temp = control.temp.take();
            control.ctx.media_root = dest.clone();
            control.set_project_path(Some(saved_path.clone()));
            control.apply_to_engine(target);
            control.ctx.sources.retain_under(&dest);
            drop(old_temp);
        }
        self.prepare_peaks(&dest, &project);
        control.mark_saved();
        Ok(saved_path)
    }

    /// Registers every media item's peak file with the peak store and starts building the ones
    /// that do not exist yet (in the background; they answer empty until written).
    pub(super) fn prepare_peaks(&self, root: &Path, project: &Project) {
        let mut builds = super::lock(&self.builds);
        builds.retain(|build| !build.is_finished());
        for item in project.media_pool.values() {
            let owpk = peak_path(root, &item.hash);
            let wav = root.join(&item.path);
            if !owpk.exists() && wav.exists() && fs::create_dir_all(root.join("cache")).is_ok() {
                builds.push(spawn_build(wav, owpk.clone()));
            }
            self.peaks.register(item.id.0, owpk);
        }
    }

    /// Waits for the peak builds started so far (tests; the app lets them finish on their own).
    #[cfg(test)]
    pub(super) fn wait_for_peaks(&self) {
        let builds = std::mem::take(&mut *super::lock(&self.builds));
        for build in builds {
            build.join().unwrap().unwrap();
        }
    }
}

#[cfg(test)]
mod bundle_path_tests {
    use super::*;

    #[test]
    fn owave_is_appended_unless_it_is_already_the_extension() {
        let name = |p: &str| {
            bundle_path(Path::new(p))
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(name("song"), "song.owave");
        assert_eq!(name("song.v2"), "song.v2.owave");
        assert_eq!(name("song.owave"), "song.owave");
        assert_eq!(name("song.OWAVE"), "song.OWAVE");
        assert_eq!(name("song.wav"), "song.wav.owave");
    }
}
