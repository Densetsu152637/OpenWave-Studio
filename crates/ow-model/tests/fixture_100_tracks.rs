//! The `100_tracks.owave` fixture (100 tracks, 500 clips) used by the arrangement frame-budget
//! benchmark, and the test that it opens. Regenerate it with
//! `cargo test -p ow-model --test fixture_100_tracks -- --ignored generate`.

use std::path::{Path, PathBuf};

use ow_model::*;

const TRACKS: usize = 100;
const CLIPS_PER_TRACK: usize = 5;
const COLORS: [u32; 6] = [0x4AA3FF, 0xFF9F43, 0x5FD38D, 0xE56BD1, 0xF2CF4B, 0x8F7BFF];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/100_tracks.owave")
}

fn build() -> Project {
    let mut ed = Editor::new(Project::new("100 tracks"));
    for i in 0..TRACKS {
        let id: TrackId = ed.alloc_id().unwrap();
        ed.apply(EditCommand::InsertTrack {
            index: i,
            bundle: Box::new(Owned::new(Track {
                id,
                name: format!("Track {}", i + 1),
                color: COLORS[i % COLORS.len()],
                kind: TrackKind::Midi,
                strip: Strip {
                    settings: StripSettings::default(),
                    sends: vec![],
                    inserts: vec![],
                },
                instrument: None,
                input: None,
                monitor: Default::default(),
            })),
        })
        .unwrap();
        for c in 0..CLIPS_PER_TRACK {
            let clip: ClipId = ed.alloc_id().unwrap();
            let notes = (0..4)
                .map(|n| Note {
                    start: f64::from(n),
                    length: 0.5,
                    key: 48 + u8::try_from((i + c + usize::try_from(n).unwrap()) % 24).unwrap(),
                    velocity: 100,
                })
                .collect();
            ed.apply(EditCommand::AddClip {
                clip: Box::new(Clip {
                    id: clip,
                    track: id,
                    name: format!("Clip {}", c + 1),
                    start: (c * 8 + i % 4) as f64,
                    length: 4.0,
                    muted: false,
                    content: ClipContent::Midi {
                        notes,
                        controllers: vec![],
                    },
                    take: None,
                }),
            })
            .unwrap();
        }
    }
    ed.project().clone()
}

#[test]
#[ignore = "writes the checked-in fixture"]
fn generate() {
    bundle::save(&fixture_dir(), &build()).unwrap();
}

#[test]
fn fixture_opens_with_100_tracks_and_500_clips() {
    let project = bundle::load(&fixture_dir()).unwrap();
    assert_eq!(project.tracks.len(), TRACKS);
    assert_eq!(project.clips.len(), TRACKS * CLIPS_PER_TRACK);
}

#[test]
fn generator_matches_the_checked_in_fixture() {
    assert_eq!(bundle::load(&fixture_dir()).unwrap(), build());
}
