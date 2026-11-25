//! Limits and hostile-input handling: id range, autosave rotation safety, `.owz` size caps,
//! Windows reserved names, sidechain cycles, pre-migration copies.

mod common;

use std::fs;
use std::io::Write;
use std::time::{Duration, UNIX_EPOCH};

use common::reference_project;
use ow_model::bundle;
use ow_model::owz::{self, Limits};
use ow_model::validate::MAX_ID;
use ow_model::*;
use zip::write::SimpleFileOptions;

fn at(seconds: u64) -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_780_000_000 + seconds)
}

fn marker(id: u64) -> Marker {
    Marker {
        id: MarkerId(id),
        name: "m".into(),
        beat: 1.0,
        color: 0,
    }
}

#[test]
fn hardening_ids_stay_within_the_javascript_safe_range() {
    let mut ed = Editor::new(Project::new("p"));
    let track = ed.alloc_track_id().unwrap();
    ed.apply(EditCommand::InsertTrack {
        index: 0,
        bundle: Box::new(Owned::new(Track {
            id: track,
            name: "t".into(),
            color: 0,
            kind: TrackKind::Midi,
            strip: Strip::default(),
            instrument: None,
            input: None,
            monitor: Default::default(),
        })),
    })
    .unwrap();
    let clip = |id: u64| EditCommand::AddClip {
        clip: Box::new(Clip {
            id: ClipId(id),
            track,
            name: "c".into(),
            start: 0.0,
            length: 1.0,
            muted: false,
            content: ClipContent::Midi {
                notes: vec![],
                controllers: vec![],
            },
            take: None,
        }),
    };
    for bad in [MAX_ID + 1, u64::MAX] {
        assert!(
            matches!(ed.apply(clip(bad)), Err(EditError::Invalid(_))),
            "{bad}"
        );
        assert!(
            ed.apply(EditCommand::AddMarker {
                marker: marker(bad)
            })
            .is_err()
        );
    }
    // The largest allowed id works and leaves the counter at its ceiling.
    ed.apply(clip(MAX_ID)).unwrap();
    assert_eq!(ed.project().next_id, MAX_ID + 1);
    ed.project().validate().unwrap();
    // Nothing is left to hand out, and nothing is ever re-issued.
    assert!(ed.alloc_id::<ClipId>().is_err());
    assert!(ed.alloc_id::<ClipId>().is_err());
    assert_eq!(ed.project().next_id, MAX_ID + 1);
}

#[test]
fn hardening_files_with_out_of_range_ids_are_rejected() {
    let base = String::from_utf8(to_json(&reference_project()).unwrap()).unwrap();
    for next_id in [u64::MAX.to_string(), (MAX_ID + 2).to_string()] {
        let text = base.replace("\"next_id\": 14", &format!("\"next_id\": {next_id}"));
        assert_ne!(text, base, "fixture has next_id 14");
        assert!(from_json(text.as_bytes()).is_err(), "{next_id}");
    }
    let mut p = Project::new("p");
    p.next_id = MAX_ID + 2;
    assert!(p.validate().is_err());
    p.next_id = MAX_ID + 1;
    p.validate().unwrap();
    p.markers.insert(MarkerId(MAX_ID + 1), marker(MAX_ID + 1));
    p.next_id = u64::MAX;
    assert!(p.validate().is_err());
}

#[test]
fn hardening_oversized_project_files_are_rejected() {
    let big = vec![b' '; format::MAX_PROJECT_BYTES + 1];
    assert!(matches!(from_json(&big), Err(FormatError::TooLarge)));
}

#[test]
fn hardening_autosave_never_prunes_the_file_it_just_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let project = reference_project();
    // Twenty backups stamped later than the autosave about to be written.
    for i in 0..20 {
        bundle::autosave(dir.path(), &project, at(10_000 + i * 60), 20).unwrap();
    }
    let written = bundle::autosave(dir.path(), &project, at(0), 20).unwrap();
    assert!(written.exists(), "the returned path exists");
    let kept = bundle::list_backups(dir.path()).unwrap();
    assert_eq!(kept.len(), 20, "still bounded");
    assert!(kept.contains(&written));
}

#[test]
fn hardening_pre_migration_copy_is_kept_outside_the_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let project = reference_project();
    bundle::save(dir.path(), &project).unwrap();
    // Same version: nothing to preserve (and saving never creates one).
    assert_eq!(
        bundle::preserve_old_version(dir.path(), FORMAT_VERSION).unwrap(),
        None
    );
    assert!(bundle::list_backups(dir.path()).unwrap().is_empty());
    // A file older than the build's version is copied byte for byte.
    let original = fs::read(dir.path().join("project.json")).unwrap();
    let copy = bundle::preserve_old_version(dir.path(), FORMAT_VERSION + 1)
        .unwrap()
        .expect("older file is preserved");
    let name = copy.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(
        name.starts_with("pre-migration-v1-") && name.ends_with(".json"),
        "{name}"
    );
    assert_eq!(fs::read(&copy).unwrap(), original);
    // It is not an autosave: rotation neither lists nor deletes it.
    assert!(bundle::list_backups(dir.path()).unwrap().is_empty());
    for i in 0..5 {
        bundle::autosave(dir.path(), &project, at(i), 1).unwrap();
    }
    assert!(copy.exists());
    // A second call never overwrites the first copy.
    let again = bundle::preserve_old_version(dir.path(), FORMAT_VERSION + 1)
        .unwrap()
        .unwrap();
    assert_ne!(again, copy);
    // No project file, nothing to preserve.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(bundle::preserve_old_version(empty.path(), 9).unwrap(), None);
}

#[test]
fn hardening_older_builds_refuse_newer_files() {
    let mut text = String::from_utf8(to_json(&reference_project()).unwrap()).unwrap();
    text = text.replace(
        "\"format_version\": 1",
        &format!("\"format_version\": {}", FORMAT_VERSION + 1),
    );
    assert!(matches!(
        from_json(text.as_bytes()),
        Err(FormatError::TooNew { found, .. }) if found == u64::from(FORMAT_VERSION) + 1
    ));
}

#[test]
fn hardening_owz_size_caps_apply_per_entry_and_in_total() {
    let root = tempfile::tempdir().unwrap();
    let good = to_json(&reference_project()).unwrap();
    let archive = root.path().join("a.owz");
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    zip.start_file("project.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&good).unwrap();
    zip.start_file("audio/a.wav", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&[0_u8; 4000]).unwrap();
    zip.start_file("audio/b.wav", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&[0_u8; 4000]).unwrap();
    zip.finish().unwrap();

    let roomy = Limits::default();
    let out = root.path().join("ok");
    owz::import_with_limits(&archive, &out, roomy).unwrap();

    let per_entry = Limits {
        max_entry_bytes: 3000,
        ..roomy
    };
    let total = Limits {
        max_total_bytes: good.len() as u64 + 5000,
        ..roomy
    };
    let few = Limits {
        max_entries: 2,
        ..roomy
    };
    for (label, limits) in [("entry", per_entry), ("total", total), ("count", few)] {
        let out = root.path().join(format!("out-{label}"));
        assert!(
            matches!(
                owz::import_with_limits(&archive, &out, limits),
                Err(BundleError::Unsafe(_))
            ),
            "{label}"
        );
        assert!(!out.exists(), "{label}: destination removed");
    }
    assert_eq!(owz::MAX_ENTRY_BYTES, 4 << 30);
    assert_eq!(owz::MAX_UNPACKED_BYTES, 32 << 30);
}

#[test]
fn hardening_windows_reserved_names_are_rejected() {
    for name in [
        "CON",
        "con.wav",
        "Prn",
        "AUX.tar.gz",
        "nul",
        "COM1.wav",
        "com9",
        "LPT0",
        "lpt5.x",
        "COM\u{b9}",
        "LPT\u{b2}.wav",
        "LPT\u{b3}",
        "con .wav",
    ] {
        let path = format!("audio/{name}");
        assert!(!validate::is_safe_media_path(&path), "{path}");
        assert!(
            !validate::is_safe_media_path(&format!("audio/sub/{name}")),
            "{path} nested"
        );
    }
    for name in [
        "console.wav",
        "com.wav",
        "COM10.wav",
        "lpt.wav",
        "auxiliary.wav",
        "a.con",
    ] {
        assert!(
            validate::is_safe_media_path(&format!("audio/{name}")),
            "{name}"
        );
    }
    let mut ed = Editor::new(Project::new("p"));
    let item = |id: u64, path: &str| MediaItem {
        id: MediaId(id),
        name: "m".into(),
        path: path.into(),
        hash: "ab".into(),
        sample_rate: 48_000,
        channels: 1,
        frames: 1,
    };
    assert!(
        ed.apply(EditCommand::AddMedia {
            item: item(1, "audio/NUL.wav")
        })
        .is_err()
    );
    ed.apply(EditCommand::AddMedia {
        item: item(1, "audio/Take.wav"),
    })
    .unwrap();
    // Paths differing only in case collide on Windows and macOS.
    let mut p = ed.project().clone();
    p.media_pool.insert(MediaId(2), item(2, "audio/TAKE.wav"));
    p.next_id = 3;
    assert!(p.validate().is_err());
    assert!(to_json(&p).is_err());
}

#[test]
fn hardening_sidechain_cycles_are_refused() {
    let mut ed = Editor::new(reference_project());
    let p = ed.project().clone();
    let keys = p.tracks.iter().find(|t| t.name == "Keys").unwrap();
    let synth = keys.instrument.unwrap();
    let bus = p.buses[0].id;
    let before = ed.project().clone();
    // The synth listening to its own track, or to the bus its track feeds, is a feedback loop.
    for source in [MixerRef::Track(keys.id), MixerRef::Bus(bus)] {
        assert_eq!(
            ed.apply(EditCommand::SetDeviceSidechain {
                device: synth,
                source: Some(source)
            }),
            Err(EditError::RoutingCycle),
            "{source:?}"
        );
        assert!(ed.project().same_content(&before));
    }
    // Listening to an unrelated strip is fine.
    let vocals = p.tracks.iter().find(|t| t.name == "Vocals").unwrap().id;
    ed.apply(EditCommand::SetDeviceSidechain {
        device: synth,
        source: Some(MixerRef::Track(vocals)),
    })
    .unwrap();
    ed.project().validate().unwrap();
    // A file with the loop is rejected on load.
    let mut looped = before.clone();
    looped.devices.get_mut(&synth).unwrap().sidechain = Some(MixerRef::Track(keys.id));
    assert!(looped.validate().is_err());
    assert!(to_json(&looped).is_err());
}
