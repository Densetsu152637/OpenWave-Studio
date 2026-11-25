//! Recording's model: track input and monitoring, takes and the active-take edit, punch range.

use ow_model::{
    BeatRange, Clip, ClipContent, EditCommand, EditError, Editor, InputChannels, MediaItem,
    MonitorMode, Owned, Project, ProjectSettings, Strip, TakeInfo, Track, TrackId, TrackInput,
    TrackKind,
};

fn editor() -> (Editor, TrackId) {
    let mut editor = Editor::new(Project::new("record"));
    let id: TrackId = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned::new(Track {
                id,
                name: "vox".into(),
                color: 0,
                kind: TrackKind::Audio,
                strip: Strip::default(),
                instrument: None,
                input: None,
                monitor: MonitorMode::Off,
            })),
        })
        .unwrap();
    let media = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::AddMedia {
            item: MediaItem {
                id: media,
                name: "take".into(),
                path: "audio/take.wav".into(),
                hash: "00".into(),
                sample_rate: 48_000,
                channels: 1,
                frames: 48_000,
                tempo: None,
                key: None,
            },
        })
        .unwrap();
    (editor, id)
}

/// Adds three takes of group 7 on `track`; the last is active.
fn add_takes(editor: &mut Editor, track: TrackId) -> Vec<Clip> {
    let media = *editor.project().media_pool.keys().next().unwrap();
    let mut clips = Vec::new();
    for index in 0..3_u32 {
        let id = editor.alloc_id().unwrap();
        let clip = Clip {
            id,
            track,
            name: format!("take {index}"),
            start: 0.0,
            length: 4.0,
            muted: false,
            content: ClipContent::Audio {
                media,
                source_offset: 0,
                gain_db: 0.0,
                fade_in: 0.0,
                fade_out: 0.0,
                warp: None,
            },
            take: Some(TakeInfo {
                group: 7,
                index,
                active: index == 2,
            }),
        };
        editor
            .apply(EditCommand::AddClip {
                clip: Box::new(clip.clone()),
            })
            .unwrap();
        clips.push(clip);
    }
    clips
}

fn active(editor: &Editor) -> Vec<u32> {
    let mut active: Vec<u32> = editor
        .project()
        .clips
        .values()
        .filter_map(|c| c.take.filter(|t| t.active).map(|t| t.index))
        .collect();
    active.sort_unstable();
    active
}

#[test]
fn input_and_monitoring_are_edits_with_undo() {
    let (mut editor, track) = editor();
    let input = Some(TrackInput {
        channels: InputChannels::Stereo(2),
    });
    editor
        .apply(EditCommand::SetTrackInput { track, input })
        .unwrap();
    editor
        .apply(EditCommand::SetTrackMonitor {
            track,
            monitor: MonitorMode::Auto,
        })
        .unwrap();
    assert_eq!(editor.project().tracks[0].input, input);
    assert_eq!(editor.project().tracks[0].monitor, MonitorMode::Auto);
    editor.undo().unwrap();
    editor.undo().unwrap();
    assert_eq!(editor.project().tracks[0].input, None);
    assert_eq!(editor.project().tracks[0].monitor, MonitorMode::Off);
    editor.redo().unwrap();
    assert_eq!(editor.project().tracks[0].input, input);
}

#[test]
fn an_input_channel_beyond_the_limit_is_refused() {
    let (mut editor, track) = editor();
    let error = editor
        .apply(EditCommand::SetTrackInput {
            track,
            input: Some(TrackInput {
                channels: InputChannels::Mono(u16::MAX),
            }),
        })
        .unwrap_err();
    assert!(matches!(error, EditError::Invalid(_)), "{error:?}");
}

#[test]
fn choosing_a_take_activates_exactly_that_take_and_undoes() {
    let (mut editor, track) = editor();
    add_takes(&mut editor, track);
    assert_eq!(active(&editor), [2]);
    editor
        .apply(EditCommand::SetActiveTake {
            track,
            group: 7,
            index: 0,
        })
        .unwrap();
    assert_eq!(active(&editor), [0]);
    editor.undo().unwrap();
    assert_eq!(active(&editor), [2]);
    editor.redo().unwrap();
    assert_eq!(active(&editor), [0]);
    let error = editor
        .apply(EditCommand::SetActiveTake {
            track,
            group: 7,
            index: 9,
        })
        .unwrap_err();
    assert!(matches!(error, EditError::NotFound { .. }), "{error:?}");
    assert_eq!(active(&editor), [0]);
}

#[test]
fn two_active_takes_in_one_group_are_refused() {
    let (mut editor, track) = editor();
    let clips = add_takes(&mut editor, track);
    let mut second_active = clips[0].clone();
    second_active.take = Some(TakeInfo {
        group: 7,
        index: 0,
        active: true,
    });
    let error = editor
        .apply(EditCommand::ReplaceClip {
            clip: Box::new(second_active),
        })
        .unwrap_err();
    assert!(matches!(error, EditError::Invalid(_)), "{error:?}");
}

#[test]
fn a_punch_range_must_end_after_it_starts() {
    let (mut editor, _) = editor();
    let settings = |start, end| ProjectSettings {
        punch_range: Some(BeatRange { start, end }),
        punch_enabled: true,
        ..ProjectSettings::default()
    };
    editor
        .apply(EditCommand::SetSettings {
            settings: settings(4.0, 8.0),
        })
        .unwrap();
    assert!(editor.project().settings.punch_enabled);
    assert!(
        editor
            .apply(EditCommand::SetSettings {
                settings: settings(8.0, 8.0),
            })
            .is_err()
    );
}
