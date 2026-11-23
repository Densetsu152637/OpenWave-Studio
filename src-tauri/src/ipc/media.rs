//! Importing audio files into the project (`media_import`).

use std::path::Path;

use ow_ipc::{EditCommand, MediaImportReply, UiCommand};
use ow_media::import::{ImportLimits, Imported, import_file};
use ow_media::peaks::Pyramid;
use ow_model::{MediaId, MediaItem};

use super::IpcState;
use super::control::{Control, PlanTarget};
use super::project::peak_path;

/// A display name the model accepts: no control characters, at most 256 characters.
pub(super) fn display_name(stem: &str) -> String {
    let name: String = stem
        .chars()
        .filter(|c| !c.is_control())
        .take(ow_model::validate::MAX_NAME_CHARS)
        .collect();
    if name.is_empty() {
        "Audio".into()
    } else {
        name
    }
}

impl IpcState {
    /// Decodes the audio file at `src`, converts it to the project's sample rate, stores it
    /// under its content hash in the media root's `audio/`, builds and registers its peaks and
    /// adds it to the media pool (the backend owns `AddMedia`; the UI then adds clips). Media
    /// that is already in the pool (the same content at the same rate) is reused, not added
    /// twice. An unsaved project imports into its temporary media folder.
    pub fn media_import(
        &self,
        src: &Path,
        target: &dyn PlanTarget,
    ) -> Result<MediaImportReply, String> {
        let mut control = super::lock(&self.control);
        let root = control.media_root_for_import()?;
        let imported = import_into(&root, src, control.session().project().settings.sample_rate)?;
        let id = self.register_media(&mut control, &root, &imported, target)?;
        Ok(MediaImportReply {
            media: id.0,
            frames: imported.frames,
            sample_rate: imported.sample_rate,
            channels: imported.channels,
        })
    }

    /// Builds the peaks of freshly imported media under `root`, registers them with the peak
    /// store and adds the media to the pool (an item already in the pool is reused).
    pub(super) fn register_media(
        &self,
        control: &mut Control,
        root: &Path,
        imported: &Imported,
        target: &dyn PlanTarget,
    ) -> Result<MediaId, String> {
        let owpk = peak_path(root, &imported.hash);
        if !owpk.exists() {
            Pyramid::from_wav(&root.join(&imported.path))
                .and_then(|pyramid| pyramid.write(&owpk))
                .map_err(|error| format!("could not build the waveform: {error}"))?;
        }

        let existing = control
            .session()
            .project()
            .media_pool
            .values()
            .find(|item| item.path == imported.path)
            .map(|item| item.id);
        let id = match existing {
            Some(id) => id,
            None => {
                let reply = control.command(UiCommand::AllocIds { count: 1 }, target)?;
                let id = MediaId(*reply.ids.first().ok_or("no media id was reserved")?);
                control.command(
                    UiCommand::Edit {
                        edit: EditCommand::AddMedia {
                            item: MediaItem {
                                id,
                                name: display_name(&imported.name),
                                path: imported.path.clone(),
                                hash: imported.hash.clone(),
                                sample_rate: imported.sample_rate,
                                channels: imported.channels,
                                frames: imported.frames,
                            },
                        },
                    },
                    target,
                )?;
                id
            }
        };
        self.peaks.register(id.0, owpk);
        Ok(id)
    }
}

/// Decodes `src` into `root`'s `audio/` at `rate` (see [`import_file`]), creating the media
/// folders first.
pub(super) fn import_into(root: &Path, src: &Path, rate: u32) -> Result<Imported, String> {
    for dir in ["audio", "cache"] {
        std::fs::create_dir_all(root.join(dir))
            .map_err(|error| format!("could not prepare the media folder: {error}"))?;
    }
    import_file(src, root, rate, &ImportLimits::default())
        .map_err(|error| format!("could not import {}: {error}", src.display()))
}
