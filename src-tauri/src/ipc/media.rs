//! Importing audio files into the project (`media_import`).

use std::path::Path;

use ow_ipc::{EditCommand, MediaImportReply, UiCommand};
use ow_media::import::{ImportLimits, Imported, import_file};
use ow_media::library::analyse_pcm;
use ow_media::peaks::Pyramid;
use ow_media::source::load_wav;
use ow_model::{KeyInfo, MediaId, MediaItem};

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

/// Longest file the import analyses for tempo and key, in seconds. Longer files (songs, field
/// recordings) are imported without; the analysis reads at most the first two minutes anyway,
/// but it first has to load the file.
pub const MAX_ANALYSIS_SECONDS: u64 = 600;

/// What import detected in a file.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Detected {
    pub tempo: Option<f64>,
    pub key: Option<KeyInfo>,
}

/// Detects the tempo and key of the imported bundle file `imported` under `root`. Returns
/// nothing for a file longer than [`MAX_ANALYSIS_SECONDS`] or one that cannot be read back;
/// analysis never fails an import.
pub(super) fn detect(root: &Path, imported: &Imported) -> Detected {
    let limit = MAX_ANALYSIS_SECONDS * u64::from(imported.sample_rate.max(1));
    if imported.frames == 0 || imported.frames > limit {
        return Detected::default();
    }
    let Ok(pcm) = load_wav(&root.join(&imported.path)) else {
        return Detected::default();
    };
    let found = analyse_pcm(&pcm);
    Detected {
        tempo: found.bpm,
        key: found.key.map(|key| KeyInfo {
            tonic: key.tonic,
            minor: key.minor,
            confidence: found.key_confidence.unwrap_or(0.0).clamp(0.0, 1.0),
        }),
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
        self.media_import_with(src, target, || {})
    }

    /// [`media_import`](Self::media_import) with a hook that runs after the analysis and before
    /// the media is registered (where a project change can slip in; tests use it to cause one).
    pub(super) fn media_import_with(
        &self,
        src: &Path,
        target: &dyn PlanTarget,
        after_detect: impl FnOnce(),
    ) -> Result<MediaImportReply, String> {
        let (root, rate) = {
            let mut control = super::lock(&self.control);
            let root = control.media_root_for_import()?;
            (root, control.session().project().settings.sample_rate)
        };
        let mut imported = import_into(&root, src, rate)?;
        // Analysis can take a few seconds; the control lock is not held for it. Media that is
        // already in the pool keeps what it has.
        let known = {
            let control = super::lock(&self.control);
            control
                .session()
                .project()
                .media_pool
                .values()
                .find(|item| item.path == imported.path)
                .cloned()
        };
        let detected = match &known {
            Some(item) => Detected {
                tempo: item.tempo,
                key: item.key,
            },
            None => detect(&root, &imported),
        };
        after_detect();
        let mut control = super::lock(&self.control);
        // The project can have been replaced, opened or saved elsewhere while the file was
        // analysed: then the media root (or the rate) is another one and the file is not in it.
        // Importing is addressed by content hash and repeatable, so import again into the
        // current root, which is the one the media item must point into.
        let now = control.media_root_for_import()?;
        let now_rate = control.session().project().settings.sample_rate;
        let root = if now != root || now_rate != rate {
            imported = import_into(&now, src, now_rate)?;
            now
        } else {
            root
        };
        let id = self.register_media_detected(&mut control, &root, &imported, detected, target)?;
        Ok(MediaImportReply {
            media: id.0,
            frames: imported.frames,
            sample_rate: imported.sample_rate,
            channels: imported.channels,
            tempo: detected.tempo,
            key: detected.key,
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
        self.register_media_detected(control, root, imported, Detected::default(), target)
    }

    /// [`register_media`](Self::register_media) with the tempo and key found in the audio, which
    /// a new item carries.
    pub(super) fn register_media_detected(
        &self,
        control: &mut Control,
        root: &Path,
        imported: &Imported,
        detected: Detected,
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
                                tempo: detected.tempo,
                                key: detected.key,
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

#[cfg(test)]
mod tests {
    use super::super::mvp::CompilingTarget;
    use super::*;

    fn write_tone(path: &Path) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..48_000_u32 {
            writer
                .write_sample(((f64::from(i) * 0.05).sin() * 8_000.0) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    /// A project replaced while the file is analysed must still end up with the media in its
    /// own media root.
    #[test]
    fn an_import_overtaken_by_a_new_project_lands_in_the_new_media_root() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("tone.wav");
        write_tone(&wav);
        let state = IpcState::new();
        let target = CompilingTarget::default();
        let reply = state
            .media_import_with(&wav, &target, || {
                state.project_new(&target).unwrap();
            })
            .unwrap();
        let control = super::super::lock(&state.control);
        let item = control
            .session()
            .project()
            .media_pool
            .get(&MediaId(reply.media))
            .expect("the media is in the new project");
        let file = control.media_root().join(&item.path);
        assert!(file.is_file(), "{} is missing", file.display());
    }
}
