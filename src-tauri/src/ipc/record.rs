//! Audio recording (`record_set_arm`, `record_start`, and recording's half of `transport_stop`).
//!
//! Arming is session state: a set of audio track ids in the [`Control`](super::control::Control)
//! shown through the mirror (`history.armedTracks`), never saved, never an undo step and never
//! dirty. `record_start` creates one take file per armed track in a private temporary folder
//! (so saving or switching projects cannot disturb it), hands the record tap to the audio
//! thread and starts the transport ([`Engine::start_recording`]). Stopping goes through
//! [`IpcState::transport_stop`]: it ends the recording, then imports each take into the media
//! root like `media_import` does (content hash into `audio/`, peaks built and registered) and
//! adds `AddMedia` and `AddClip` for every take inside one transaction named "Record". A take
//! starts at the recording start minus the stream's round-trip latency (see
//! `ow_engine::record`), converted to beats with the project's tempo map.
//!
//! Plain methods on [`IpcState`] (testable with a mock backend and no window); the commands at
//! the end only run them on the blocking pool.

use std::path::PathBuf;

use ow_engine::{Engine, FinishedTake, Recording, TransportCommand};
use ow_ipc::{EditCommand, UiCommand};
use ow_media::import::Imported;
use ow_model::{Clip, ClipContent, ClipId, TrackId};
use tauri::{AppHandle, Manager};

use super::control::Control;
use super::media::{display_name, import_into};
use super::{IpcState, blocking, lock};
use crate::engine::EngineState;

/// A recording in progress: the engine's handle, the folder holding the take files (deleted
/// once the takes are in the project) and the project it belongs to.
pub(super) struct ActiveRecording {
    recording: Recording,
    dir: tempfile::TempDir,
    epoch: u64,
}

impl IpcState {
    /// Arms or disarms an audio track for recording. Refused while recording and for a track
    /// that is not an audio track.
    pub fn record_set_arm(&self, track: u64, armed: bool) -> Result<(), String> {
        if lock(&self.recording).is_some() {
            return Err("stop recording before changing which tracks are armed".into());
        }
        lock(&self.control).set_armed(track, armed)
    }

    /// Refuses to replace the project while a recording is running: its takes belong to it.
    pub(super) fn refuse_while_recording(&self) -> Result<(), String> {
        if lock(&self.recording).is_some() {
            return Err("Stop recording before opening or creating a project".into());
        }
        Ok(())
    }

    /// Starts recording on every armed track from the current position: the takes are written
    /// as the transport plays and become clips when the transport stops.
    pub fn record_start(&self, engine: &Engine) -> Result<(), String> {
        let mut slot = lock(&self.recording);
        if slot.is_some() {
            return Err("already recording".into());
        }
        let (tracks, epoch) = {
            let control = lock(&self.control);
            (control.armed_audio_tracks(), control.epoch())
        };
        if tracks.is_empty() {
            return Err("arm an audio track to record on first".into());
        }
        let dir = tempfile::Builder::new()
            .prefix("openwave-take-")
            .tempdir()
            .map_err(|error| format!("could not create a folder for the recording: {error}"))?;
        let takes: Vec<(TrackId, PathBuf)> = tracks
            .into_iter()
            .map(|track| (track, dir.path().join(format!("take-{}.wav", track.0))))
            .collect();
        let recording = engine
            .start_recording(&takes)
            .map_err(|error| error.to_string())?;
        *slot = Some(ActiveRecording {
            recording,
            dir,
            epoch,
        });
        Ok(())
    }

    /// Stops the transport. When recording, also finishes the takes and adds them to the
    /// project (see the module documentation); a problem with the takes is reported after the
    /// ones that could be added were.
    pub fn transport_stop(&self, engine: &Engine) -> Result<(), String> {
        let active = lock(&self.recording).take();
        match active {
            None if engine.send_transport(TransportCommand::Stop) => Ok(()),
            None => Err("the audio engine is not running".into()),
            Some(active) => self.finish_recording(active, engine),
        }
    }

    fn finish_recording(&self, active: ActiveRecording, engine: &Engine) -> Result<(), String> {
        let ActiveRecording {
            recording,
            dir,
            epoch,
        } = active;
        let mut problems = Vec::new();
        match engine.stop_recording(recording) {
            Ok(stopped) => {
                if !stopped.complete {
                    problems.push(
                        "part of the recording may be missing (the audio engine did not stop \
                         promptly or skipped input)"
                            .to_string(),
                    );
                }
                for failure in &stopped.failures {
                    problems.push(format!(
                        "the take for track {} could not be finished ({}): {}",
                        failure.track.0,
                        failure.path.display(),
                        failure.error
                    ));
                }
                let takes: Vec<FinishedTake> =
                    stopped.takes.into_iter().filter(|t| t.frames > 0).collect();
                if takes.iter().any(|take| take.dropped_frames > 0) {
                    problems.push(
                        "the disk could not keep up and part of the recording is missing".into(),
                    );
                }
                self.add_takes(takes, epoch, engine, &mut problems);
            }
            Err(error) => problems.push(error.to_string()),
        }
        if problems.is_empty() {
            return Ok(());
        }
        // Whatever went wrong, the take files stay so nothing recorded is lost.
        let kept = dir.keep();
        Err(format!(
            "{}; the take files were kept in {}",
            problems.join("; "),
            kept.display()
        ))
    }

    /// Imports the takes and adds them to the project in one transaction. Failures are
    /// appended to `problems`; whatever succeeded is kept, and the transaction is always ended.
    fn add_takes(
        &self,
        takes: Vec<FinishedTake>,
        epoch: u64,
        engine: &Engine,
        problems: &mut Vec<String>,
    ) {
        if takes.is_empty() {
            return;
        }
        let mut control = lock(&self.control);
        if control.epoch() != epoch {
            problems.push("the project changed while recording".into());
            return;
        }
        let root = match control.media_root_for_import() {
            Ok(root) => root,
            Err(error) => {
                problems.push(error);
                return;
            }
        };
        let rate = control.session().project().settings.sample_rate;
        let mut imported = Vec::with_capacity(takes.len());
        for take in takes {
            match import_into(&root, &take.path, rate) {
                Ok(file) => imported.push((take, file)),
                Err(error) => problems.push(format!("{}: {error}", take.path.display())),
            }
        }
        if imported.is_empty() {
            return;
        }
        let begin = UiCommand::Begin {
            label: "Record".into(),
        };
        if let Err(error) = control.command(begin, engine) {
            problems.push(error);
            return;
        }
        for (take, file) in &imported {
            if let Err(error) = self.place_take(&mut control, &root, take, file, engine) {
                problems.push(error);
            }
        }
        // Commit what was added, even when a take failed on the way.
        if let Err(error) = control.command(UiCommand::End, engine) {
            problems.push(error);
        }
    }

    /// Adds one take to the media pool and as a clip on its track.
    fn place_take(
        &self,
        control: &mut Control,
        root: &std::path::Path,
        take: &FinishedTake,
        file: &Imported,
        engine: &Engine,
    ) -> Result<(), String> {
        let Some(track) = control
            .session()
            .project()
            .tracks
            .iter()
            .find(|t| t.id == take.track)
            .cloned()
        else {
            return Err("a recorded track was deleted; its take was not added".to_string());
        };
        let tempo = &control.session().project().tempo_map;
        let rate = take.sample_rate;
        let start = tempo.samples_to_beats(take.start_sample as f64, rate);
        let end = tempo.samples_to_beats((take.start_sample + take.frames) as f64, rate);
        let media = self.register_media(control, root, file, engine)?;
        let reply = control.command(UiCommand::AllocIds { count: 1 }, engine)?;
        let id = *reply.ids.first().ok_or("no clip id was reserved")?;
        control.command(
            UiCommand::Edit {
                edit: EditCommand::AddClip {
                    clip: Box::new(Clip {
                        id: ClipId::from(id),
                        track: track.id,
                        name: display_name(&format!("{} take", track.name)),
                        start,
                        length: end - start,
                        muted: false,
                        content: ClipContent::Audio {
                            media,
                            source_offset: 0,
                            gain_db: 0.0,
                            fade_in: 0.0,
                            fade_out: 0.0,
                        },
                        take: None,
                    }),
                },
            },
            engine,
        )?;
        Ok(())
    }
}

/// Arms or disarms the audio track `track` for recording.
#[tauri::command]
pub async fn record_set_arm(app: AppHandle, track: u64, armed: bool) -> Result<(), String> {
    blocking(move || app.state::<IpcState>().record_set_arm(track, armed)).await
}

/// Starts recording on the armed tracks; stop with `transport_stop`.
#[tauri::command]
pub async fn record_start(app: AppHandle) -> Result<(), String> {
    blocking(move || {
        let state = app.state::<IpcState>();
        let engine = app.state::<EngineState>();
        state.record_start(engine.engine())
    })
    .await
}
