//! MIDI clips: note operations (`midi_edit`) and Standard MIDI Files (`midi_import`,
//! `midi_export`).
//!
//! Plain methods on [`IpcState`] (testable without a window); the commands at the end only run
//! them on the blocking pool. Every change is one undo step.
//!
//! * `midi_edit` runs one [`MidiEdit`] (quantize, legato, humanize, transpose, velocity) on a
//!   MIDI clip's notes, or on the notes at some indices, and replaces the clip.
//! * `midi_import` reads a `.mid` file (format 0 or 1) and adds one clip per file track that
//!   holds notes or control changes: onto new MIDI tracks (each with a `builtin.synth`), or all
//!   onto one existing MIDI track. Clips start at the first event rounded down to a beat, plus
//!   `start_beat`. The file's tempo changes replace the project's tempo map only on request.
//! * `midi_export` writes MIDI clips as a format 1 file: a conductor track with the project's
//!   tempo points and first time signature, then one track per MIDI track, at the clips'
//!   positions, with each note cut at its clip's end as it would play.
//!
//! Files are `.mid` or `.midi` only, at most [`MAX_FILE_BYTES`] when read, and an existing file is
//! replaced only on request.

use std::path::{Path, PathBuf};

use ow_ipc::{EditCommand, MidiEditReply, MidiExportReply, MidiImportReply, UiCommand};
use ow_model::midi::{self, ExportTrack, MidiEdit};
use ow_model::time::{TempoMap, TempoPoint};
use ow_model::{
    Clip, ClipContent, ClipId, Controller, Device, DeviceId, Note, Owned, Strip, Track, TrackId,
    TrackKind,
};
use tauri::{AppHandle, Manager};

use super::control::{Control, PlanTarget};
use super::media::display_name;
use super::{IpcState, blocking, lock};
use crate::engine::EngineState;

/// Largest MIDI file read.
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Color of tracks created by an import (`0xRRGGBB`).
const NEW_TRACK_COLOR: u32 = 0x004F_9DFF;

/// True for a path that ends in `.mid` or `.midi` (any case).
pub fn is_midi_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("mid") || extension.eq_ignore_ascii_case("midi")
    })
}

fn require_midi_path(path: &Path) -> Result<(), String> {
    if is_midi_path(path) {
        Ok(())
    } else {
        Err("the path must end in .mid or .midi".into())
    }
}

/// The notes and controllers of a MIDI clip as they play: positions moved by the clip's start,
/// notes cut at its end.
fn played(clip: &Clip) -> (Vec<Note>, Vec<Controller>) {
    let ClipContent::Midi { notes, controllers } = &clip.content else {
        return (Vec::new(), Vec::new());
    };
    let notes = notes
        .iter()
        .filter(|n| n.start < clip.length)
        .map(|n| Note {
            start: clip.start + n.start,
            length: n.length.min(clip.length - n.start),
            ..*n
        })
        .collect();
    let controllers = controllers
        .iter()
        .filter(|c| c.beat < clip.length)
        .map(|c| Controller {
            beat: clip.start + c.beat,
            ..*c
        })
        .collect();
    (notes, controllers)
}

impl IpcState {
    /// Applies `edit` to the notes of MIDI clip `clip` (only the notes at the indices in
    /// `selection`, when given) as one undo step.
    pub fn midi_edit(
        &self,
        clip: u64,
        edit: &MidiEdit,
        selection: Option<&[usize]>,
        target: &dyn PlanTarget,
    ) -> Result<MidiEditReply, String> {
        let mut control = lock(&self.control);
        let found = control
            .session()
            .project()
            .clips
            .get(&ClipId(clip))
            .cloned()
            .ok_or_else(|| format!("there is no clip {clip}"))?;
        let ClipContent::Midi { notes, controllers } = &found.content else {
            return Err(format!("clip {clip} is not a MIDI clip"));
        };
        let edited = edit.apply(notes, selection);
        let reply = MidiEditReply {
            clip,
            notes_before: notes.len() as u32,
            notes_after: edited.len() as u32,
        };
        let mut replacement = found.clone();
        replacement.content = ClipContent::Midi {
            notes: edited,
            controllers: controllers.clone(),
        };
        control.command(
            UiCommand::Begin {
                label: edit.label().into(),
            },
            target,
        )?;
        let applied = control.command(
            UiCommand::Edit {
                edit: EditCommand::ReplaceClip {
                    clip: Box::new(replacement),
                },
            },
            target,
        );
        control.command(UiCommand::End, target)?;
        applied?;
        Ok(reply)
    }

    /// Reads the MIDI file at `path` and adds its tracks as clips (see the module docs).
    /// `track` names an existing MIDI track to put every clip on; otherwise each file track gets
    /// a new MIDI track with a synth. `start_beat` moves everything later. With `apply_tempo`
    /// the file's tempo changes become the project's tempo map.
    pub fn midi_import(
        &self,
        path: &Path,
        track: Option<u64>,
        start_beat: f64,
        apply_tempo: bool,
        target: &dyn PlanTarget,
    ) -> Result<MidiImportReply, String> {
        require_midi_path(path)?;
        if !(start_beat.is_finite() && start_beat >= 0.0) {
            return Err("start_beat must be 0 or more".into());
        }
        let size = std::fs::metadata(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?
            .len();
        if size > MAX_FILE_BYTES {
            return Err(format!(
                "{} is larger than the {} MiB limit",
                path.display(),
                MAX_FILE_BYTES / (1024 * 1024)
            ));
        }
        let bytes = std::fs::read(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let file = midi::import(&bytes).map_err(|error| error.to_string())?;
        if file.tracks.is_empty() {
            return Err("the file has no notes or control changes".into());
        }
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut control = lock(&self.control);
        if let Some(id) = track {
            let is_midi = control
                .session()
                .project()
                .tracks
                .iter()
                .any(|t| t.id == TrackId(id) && t.kind == TrackKind::Midi);
            if !is_midi {
                return Err(format!("track {id} is not a MIDI track"));
            }
        }
        let tempo_map = if apply_tempo && !file.tempo.is_empty() {
            let mut points: Vec<TempoPoint> = file
                .tempo
                .iter()
                .map(|(beat, bpm)| {
                    TempoPoint::constant(
                        *beat,
                        bpm.clamp(ow_model::time::BPM_MIN, ow_model::time::BPM_MAX),
                    )
                })
                .collect();
            if points[0].beat > 0.0 {
                points.insert(0, TempoPoint::constant(0.0, points[0].bpm));
            }
            Some(
                TempoMap::new(points)
                    .map_err(|error| format!("the file's tempo map: {error:?}"))?,
            )
        } else {
            None
        };

        import_atomically(
            &mut control,
            &file,
            track,
            start_beat,
            &stem,
            tempo_map,
            target,
        )
    }

    /// Writes MIDI clips to `path` as a format 1 file. `clips` are clip ids, `tracks` are track
    /// ids (all their MIDI clips); with neither, every MIDI track. An existing file is
    /// replaced only with `overwrite`.
    pub fn midi_export(
        &self,
        path: &Path,
        clips: &[u64],
        tracks: &[u64],
        overwrite: bool,
    ) -> Result<MidiExportReply, String> {
        require_midi_path(path)?;
        if path.exists() && !overwrite {
            return Err(format!(
                "{} already exists; pass overwrite=true to replace it",
                path.display()
            ));
        }
        let (project, _) = lock(&self.control).offline_snapshot();
        let midi_tracks: Vec<&Track> = project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Midi)
            .collect();
        let wanted: Vec<&Track> = if !clips.is_empty() {
            let mut wanted = Vec::new();
            for id in clips {
                let clip = project
                    .clips
                    .get(&ClipId(*id))
                    .ok_or_else(|| format!("there is no clip {id}"))?;
                let track = midi_tracks
                    .iter()
                    .find(|t| t.id == clip.track)
                    .ok_or_else(|| format!("clip {id} is not on a MIDI track"))?;
                if !wanted.iter().any(|t: &&Track| t.id == track.id) {
                    wanted.push(*track);
                }
            }
            wanted
        } else if !tracks.is_empty() {
            tracks
                .iter()
                .map(|id| {
                    midi_tracks
                        .iter()
                        .find(|t| t.id.0 == *id)
                        .copied()
                        .ok_or_else(|| format!("track {id} is not a MIDI track"))
                })
                .collect::<Result<_, _>>()?
        } else {
            midi_tracks
        };
        if wanted.is_empty() {
            return Err("there are no MIDI tracks to export".into());
        }
        let mut contents: Vec<(String, Vec<Note>, Vec<Controller>)> = Vec::new();
        for track in &wanted {
            let mut notes = Vec::new();
            let mut controllers = Vec::new();
            for clip in project.clips.values().filter(|c| c.track == track.id) {
                if !clips.is_empty() && !clips.contains(&clip.id.0) {
                    continue;
                }
                // A muted clip is silent, so it is exported only when asked for by id.
                if clip.muted && !clips.contains(&clip.id.0) {
                    continue;
                }
                let (n, c) = played(clip);
                notes.extend(n);
                controllers.extend(c);
            }
            notes.sort_by(|a, b| a.start.total_cmp(&b.start));
            controllers.sort_by(|a, b| a.beat.total_cmp(&b.beat));
            contents.push((track.name.clone(), notes, controllers));
        }
        let export_tracks: Vec<ExportTrack<'_>> = contents
            .iter()
            .map(|(name, notes, controllers)| ExportTrack {
                name,
                notes,
                controllers,
            })
            .collect();
        let bytes = midi::export(
            &export_tracks,
            &project.tempo_map,
            project.time_signature_map.signature_at_bar(0),
        )
        .map_err(|error| error.to_string())?;
        write_atomically(path, &bytes)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        Ok(MidiExportReply {
            path: path.to_string_lossy().into_owned(),
            tracks: contents.len() as u32,
            notes: contents
                .iter()
                .map(|(_, notes, _)| notes.len() as u32)
                .sum(),
        })
    }
}

/// Runs the import as one "Import MIDI" step; when anything fails the transaction is cancelled,
/// so nothing of a partial import stays in the project.
pub(super) fn import_atomically(
    control: &mut Control,
    file: &midi::ImportedMidi,
    track: Option<u64>,
    start_beat: f64,
    stem: &str,
    tempo_map: Option<TempoMap>,
    target: &dyn PlanTarget,
) -> Result<MidiImportReply, String> {
    control.command(
        UiCommand::Begin {
            label: "Import MIDI".into(),
        },
        target,
    )?;
    match import_tracks(control, file, track, start_beat, stem, tempo_map, target) {
        Ok(reply) => match control.command(UiCommand::End, target) {
            Ok(_) => Ok(reply),
            Err(error) => {
                let _ = control.command(UiCommand::Cancel, target);
                Err(error)
            }
        },
        Err(error) => {
            let _ = control.command(UiCommand::Cancel, target);
            Err(error)
        }
    }
}

/// Adds the file's tracks inside the open "Import MIDI" transaction.
fn import_tracks(
    control: &mut Control,
    file: &midi::ImportedMidi,
    track: Option<u64>,
    start_beat: f64,
    stem: &str,
    tempo_map: Option<TempoMap>,
    target: &dyn PlanTarget,
) -> Result<MidiImportReply, String> {
    let mut reply = MidiImportReply {
        tracks: Vec::new(),
        clips: Vec::new(),
        tempo_applied: false,
    };
    if let Some(tempo_map) = tempo_map {
        control.command(
            UiCommand::Edit {
                edit: EditCommand::SetTempoMap { tempo_map },
            },
            target,
        )?;
        reply.tempo_applied = true;
    }
    let alloc = |control: &mut Control| -> Result<u64, String> {
        let ids = control
            .command(UiCommand::AllocIds { count: 1 }, target)?
            .ids;
        ids.first()
            .copied()
            .ok_or_else(|| "no id was reserved".to_string())
    };
    for (index, imported) in file.tracks.iter().enumerate() {
        let name = if imported.name.is_empty() {
            if file.tracks.len() > 1 {
                format!("{stem} {}", index + 1)
            } else {
                stem.to_string()
            }
        } else {
            imported.name.clone()
        };
        let name = display_name(&name);
        let start = imported.start();
        let clip_id = ClipId(alloc(control)?);
        let track_id = match track {
            Some(id) => TrackId(id),
            None => {
                let id = TrackId(alloc(control)?);
                let synth = DeviceId(alloc(control)?);
                let at = control.session().project().tracks.len();
                control.command(
                    UiCommand::Edit {
                        edit: EditCommand::InsertTrack {
                            index: at,
                            bundle: Box::new(Owned {
                                item: Track {
                                    id,
                                    name: name.clone(),
                                    color: NEW_TRACK_COLOR,
                                    kind: TrackKind::Midi,
                                    strip: Strip::default(),
                                    instrument: Some(synth),
                                    input: None,
                                    monitor: Default::default(),
                                },
                                clips: Vec::new(),
                                devices: vec![Device {
                                    id: synth,
                                    kind: "builtin.synth".into(),
                                    name: "Synth".into(),
                                    enabled: true,
                                    params: Default::default(),
                                    state: Vec::new(),
                                    sidechain: None,
                                }],
                                lanes: Vec::new(),
                            }),
                        },
                    },
                    target,
                )?;
                reply.tracks.push(id.0);
                id
            }
        };
        let notes = imported
            .notes
            .iter()
            .map(|n| Note {
                start: n.start - start,
                ..*n
            })
            .collect();
        let controllers = imported
            .controllers
            .iter()
            .map(|c| Controller {
                beat: c.beat - start,
                ..*c
            })
            .collect();
        control.command(
            UiCommand::Edit {
                edit: EditCommand::AddClip {
                    clip: Box::new(Clip {
                        id: clip_id,
                        track: track_id,
                        name,
                        start: start + start_beat,
                        length: imported.length(),
                        muted: false,
                        content: ClipContent::Midi { notes, controllers },
                        take: None,
                    }),
                },
            },
            target,
        )?;
        reply.clips.push(clip_id.0);
    }
    Ok(reply)
}

/// Writes `bytes` to `path` through a temporary file that replaces it only once complete.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".writing");
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, bytes)
        .and_then(|()| std::fs::rename(&temp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })
}

/// Applies a note operation to a MIDI clip (one undo step); `notes` limits it to the notes at
/// those indices.
#[tauri::command]
pub async fn midi_edit(
    app: AppHandle,
    clip: u64,
    edit: MidiEdit,
    notes: Option<Vec<usize>>,
) -> Result<MidiEditReply, String> {
    blocking(move || {
        let engine = app.state::<EngineState>();
        app.state::<IpcState>()
            .midi_edit(clip, &edit, notes.as_deref(), engine.engine())
    })
    .await
}

/// Imports a `.mid` file as clips (one undo step).
#[tauri::command]
pub async fn midi_import(
    app: AppHandle,
    path: String,
    track: Option<u64>,
    start_beat: Option<f64>,
    apply_tempo: Option<bool>,
) -> Result<MidiImportReply, String> {
    blocking(move || {
        let engine = app.state::<EngineState>();
        app.state::<IpcState>().midi_import(
            Path::new(&path),
            track,
            start_beat.unwrap_or(0.0),
            apply_tempo.unwrap_or(false),
            engine.engine(),
        )
    })
    .await
}

/// Exports MIDI clips (`clips`), the MIDI clips of tracks (`tracks`) or every MIDI track to a
/// `.mid` file.
#[tauri::command]
pub async fn midi_export(
    app: AppHandle,
    path: String,
    clips: Option<Vec<u64>>,
    tracks: Option<Vec<u64>>,
    overwrite: Option<bool>,
) -> Result<MidiExportReply, String> {
    blocking(move || {
        app.state::<IpcState>().midi_export(
            Path::new(&path),
            clips.as_deref().unwrap_or(&[]),
            tracks.as_deref().unwrap_or(&[]),
            overwrite.unwrap_or(false),
        )
    })
    .await
}
