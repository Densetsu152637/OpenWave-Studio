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
//! The project decides how a recording runs: each track records the input channels its
//! `input` names (default: channel 0, or the pair 0 and 1 on a wider device); an enabled punch
//! range records only inside it; `count_in_bars` counts in (clicks sound, nothing is recorded);
//! and with a loop range set every pass of the loop becomes a take (at most
//! `ow_engine::MAX_LOOP_TAKES`; later passes are reported as a problem). The takes of one
//! track's loop recording are added as clips that share a take group (`Clip::take`); the last
//! pass is the active take and the others are silent until `SetActiveTake` chooses one.
//!
//! Plain methods on [`IpcState`] (testable with a mock backend and no window); the commands at
//! the end only run them on the blocking pool.

use std::collections::BTreeMap;

use ow_engine::{
    Engine, FinishedTake, MAX_LOOP_TAKES, RecordPlan, Recording, TrackTakes, TransportCommand,
};
use ow_ipc::{EditCommand, UiCommand};
use ow_media::import::Imported;
use ow_model::{Clip, ClipContent, ClipId, InputChannels, TakeInfo, TrackId};
use tauri::{AppHandle, Manager};

use super::control::Control;
use super::media::{display_name, import_into};
use super::{IpcState, blocking, lock};
use crate::engine::EngineState;

/// Most take writers a loop recording prepares across all armed tracks (each is a thread, a
/// ring of about 0.4 to 0.8 MB and a file); a track gets at least 2 passes and at most
/// `MAX_LOOP_TAKES`, so 16 armed tracks keep 4 passes each and 33 or more keep 2.
const MAX_PREPARED_TAKES: usize = 64;

/// A recording in progress: the engine's handle, the folder holding the take files (deleted
/// once the takes are in the project) and the project it belongs to.
pub(super) struct ActiveRecording {
    recording: Recording,
    dir: tempfile::TempDir,
    epoch: u64,
    /// The recording loops, so its takes are one take group per track.
    looped: bool,
}

/// Where a take sits in its group when it becomes a clip.
struct TakeSeed {
    /// The group's id, once the group's first clip has one (that clip's id).
    group: Option<u64>,
    index: u32,
    active: bool,
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
        let (tracks, settings, epoch) = {
            let control = lock(&self.control);
            let project = control.session().project();
            let armed = control.armed_audio_tracks();
            let tracks: Vec<(TrackId, Option<(u16, u16)>)> = armed
                .into_iter()
                .map(|id| {
                    let input = project
                        .tracks
                        .iter()
                        .find(|t| t.id == id)
                        .and_then(|t| t.input)
                        .map(|input| match input.channels {
                            InputChannels::Mono(first) => (first, 1),
                            InputChannels::Stereo(first) => (first, 2),
                        });
                    (id, input)
                })
                .collect();
            (tracks, project.settings.clone(), control.epoch())
        };
        if tracks.is_empty() {
            return Err("arm an audio track to record on first".into());
        }
        let dir = tempfile::Builder::new()
            .prefix("openwave-take-")
            .tempdir()
            .map_err(|error| format!("could not create a folder for the recording: {error}"))?;
        let looped = settings.loop_range.is_some();
        // Every pass needs a writer thread, ring and file up front. Keep the total bounded:
        // 16 passes for a few tracks, fewer (never below 2) as more tracks are armed.
        let passes = if looped {
            (MAX_PREPARED_TAKES / tracks.len().max(1)).clamp(2, MAX_LOOP_TAKES)
        } else {
            1
        };
        let plan = RecordPlan {
            tracks: tracks
                .into_iter()
                .map(|(track, input)| TrackTakes {
                    track,
                    input,
                    paths: (0..passes)
                        .map(|pass| dir.path().join(format!("take-{}-{pass}.wav", track.0)))
                        .collect(),
                })
                .collect(),
            loop_beats: settings.loop_range.map(|range| (range.start, range.end)),
            punch_beats: settings
                .punch_range
                .filter(|_| settings.punch_enabled)
                .map(|range| (range.start, range.end)),
            count_in_bars: u32::from(settings.count_in_bars),
        };
        let recording = engine
            .start_recording_plan(&plan)
            .map_err(|error| error.to_string())?;
        *slot = Some(ActiveRecording {
            recording,
            dir,
            epoch,
            looped,
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
            looped,
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
                if stopped.dropped_passes > 0 {
                    problems.push(format!(
                        "{} loop pass(es) beyond the takes prepared for each track (at most {MAX_LOOP_TAKES}) \
                         were not recorded",
                        stopped.dropped_passes
                    ));
                }
                let takes: Vec<FinishedTake> =
                    stopped.takes.into_iter().filter(|t| t.frames > 0).collect();
                if takes.iter().any(|take| take.dropped_frames > 0) {
                    problems.push(
                        "the disk could not keep up and part of the recording is missing".into(),
                    );
                }
                self.add_takes(takes, epoch, looped, engine, &mut problems);
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
        looped: bool,
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
        // A loop recording's takes are a group per track: the last pass plays, the others wait.
        let mut frames: BTreeMap<TrackId, Vec<u64>> = BTreeMap::new();
        for (take, _) in &imported {
            frames.entry(take.track).or_default().push(take.frames);
        }
        let active_pass: BTreeMap<TrackId, usize> = frames
            .iter()
            .map(|(track, frames)| (*track, active_pass_index(frames)))
            .collect();
        let mut groups: BTreeMap<TrackId, (Option<u64>, u32)> = BTreeMap::new();
        let mut placed: BTreeMap<TrackId, Vec<ClipId>> = BTreeMap::new();
        let mut pass_of: BTreeMap<TrackId, usize> = BTreeMap::new();
        for (take, file) in &imported {
            let pass = {
                let seen = pass_of.entry(take.track).or_default();
                *seen += 1;
                *seen - 1
            };
            let seed = looped.then(|| {
                let (group, index) = groups.entry(take.track).or_insert((None, 0));
                TakeSeed {
                    group: *group,
                    index: *index,
                    active: pass == active_pass[&take.track],
                }
            });
            match self.place_take(&mut control, &root, take, file, seed, engine) {
                Ok(clip) => {
                    if looped {
                        let (group, index) = groups.entry(take.track).or_insert((None, 0));
                        group.get_or_insert(clip.0);
                        *index += 1;
                        placed.entry(take.track).or_default().push(clip);
                    }
                }
                Err(error) => problems.push(error),
            }
        }
        for clips in placed.values() {
            if let Err(error) = ensure_active_take(&mut control, clips, engine) {
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
        seed: Option<TakeSeed>,
        engine: &Engine,
    ) -> Result<ClipId, String> {
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
        let name = match &seed {
            Some(seed) => format!("{} take {}", track.name, seed.index + 1),
            None => format!("{} take", track.name),
        };
        control.command(
            UiCommand::Edit {
                edit: EditCommand::AddClip {
                    clip: Box::new(Clip {
                        id: ClipId::from(id),
                        track: track.id,
                        name: display_name(&name),
                        start,
                        length: end - start,
                        muted: false,
                        content: ClipContent::Audio {
                            media,
                            source_offset: 0,
                            gain_db: 0.0,
                            fade_in: 0.0,
                            fade_out: 0.0,
                            warp: None,
                        },
                        take: seed.map(|seed| TakeInfo {
                            group: seed.group.unwrap_or(id),
                            index: seed.index,
                            active: seed.active,
                        }),
                    }),
                },
            },
            engine,
        )?;
        Ok(ClipId::from(id))
    }
}

/// The pass of a loop recording that plays: the last one, unless Stop cut it short (it is
/// shorter than the longest pass), then the last complete pass. `frames` is each pass's length.
pub(super) fn active_pass_index(frames: &[u64]) -> usize {
    let longest = frames.iter().copied().max().unwrap_or(0);
    match frames.last() {
        Some(&last) if last * 100 < longest * 98 => frames
            .iter()
            .rposition(|&f| f * 100 >= longest * 98)
            .unwrap_or(frames.len() - 1),
        _ => frames.len().saturating_sub(1),
    }
}

/// A loop recording's placed takes (in pass order) need one active take: when the last pass
/// failed to place, none is, and the recording would be silent, so the last placed one plays.
pub(super) fn ensure_active_take(
    control: &mut Control,
    clips: &[ClipId],
    engine: &Engine,
) -> Result<(), String> {
    let project = control.session().project();
    let Some(last) = clips.last().and_then(|id| project.clips.get(id)).cloned() else {
        return Ok(());
    };
    let active = clips
        .iter()
        .filter_map(|id| project.clips.get(id))
        .any(|clip| clip.take.is_some_and(|take| take.active));
    let Some(mut take) = last.take else {
        return Ok(());
    };
    if active {
        return Ok(());
    }
    take.active = true;
    let mut clip = last;
    clip.take = Some(take);
    control.command(
        UiCommand::Edit {
            edit: EditCommand::ReplaceClip {
                clip: Box::new(clip),
            },
        },
        engine,
    )?;
    Ok(())
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

#[cfg(test)]
mod active_pass_tests {
    use super::active_pass_index;

    #[test]
    fn the_last_pass_plays_unless_stop_cut_it_short() {
        assert_eq!(active_pass_index(&[]), 0);
        assert_eq!(active_pass_index(&[100]), 0);
        assert_eq!(active_pass_index(&[100, 100, 100]), 2);
        assert_eq!(active_pass_index(&[100, 100, 50]), 1);
        assert_eq!(active_pass_index(&[100, 80, 50]), 0);
        assert_eq!(active_pass_index(&[50, 100]), 1);
        // Buffer-size jitter does not demote a complete last pass.
        assert_eq!(active_pass_index(&[480_000, 480_000, 479_992]), 2);
        assert_eq!(active_pass_index(&[480_000, 480_000, 400_000]), 1);
    }
}
