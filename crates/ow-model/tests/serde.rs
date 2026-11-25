//! Serialization: round trips, the committed golden file of every format version, migration,
//! and rejection of malformed or hostile input without panicking.

mod common;

use std::path::PathBuf;

use common::{make_command, ops, reference_project};
use ow_model::migrate::{Migration, migrate_chain};
use ow_model::*;
use proptest::prelude::*;
use serde_json::{Value, json};

fn golden_path(version: u32) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("v{version}"))
        .join("project.json")
}

/// The committed file with line endings normalised (Windows checkouts may convert them).
fn golden_v1() -> Vec<u8> {
    let text = std::fs::read_to_string(golden_path(1)).expect("golden file");
    // `lines` strips a CR before each LF; the file ends with exactly one newline.
    let mut normalised = text.lines().collect::<Vec<_>>().join("\n");
    normalised.push('\n');
    normalised.into_bytes()
}

/// `OW_UPDATE_GOLDEN=1 cargo test -p ow-model serde_golden_v1` rewrites the fixture. Never do
/// that for a version that has shipped: the file is what old projects look like.
#[test]
fn serde_golden_v1_loads() {
    if std::env::var_os("OW_UPDATE_GOLDEN").is_some() {
        let path = golden_path(1);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, to_json(&reference_project()).unwrap()).unwrap();
    }
    let loaded = from_json(&golden_v1()).expect("the committed v1 file loads");
    assert_eq!(loaded, reference_project());
    assert_eq!(loaded.tracks.len(), 2);
    assert_eq!(loaded.devices.len(), 2);
    assert_eq!(loaded.clips.len(), 3);
    // Reading the golden file and writing it again is stable.
    assert_eq!(to_json(&loaded).unwrap(), golden_v1());
}

#[test]
fn serde_round_trip_of_the_reference_project() {
    let project = reference_project();
    let bytes = to_json(&project).unwrap();
    assert_eq!(from_json(&bytes).unwrap(), project);
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["format_version"], json!(FORMAT_VERSION));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any project reachable through edit commands survives save and load unchanged.
    #[test]
    fn serde_round_trip_of_random_projects(seq in ops(60)) {
        let mut ed = Editor::new(Project::new("random"));
        for op in seq {
            if let Some(command) = make_command(&mut ed, op) {
                let _ = ed.apply(command);
            }
        }
        let project = ed.project();
        let bytes = to_json(project).unwrap();
        let loaded = from_json(&bytes).unwrap();
        prop_assert_eq!(&loaded, project);
    }

    /// Corrupting the golden file byte by byte never panics, and whatever still loads is valid.
    #[test]
    fn serde_corrupted_files_never_panic(
        edits in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>(), 0_u8..3), 1..6),
    ) {
        let mut bytes = golden_v1();
        for (index, byte, kind) in edits {
            let at = index.index(bytes.len());
            match kind {
                0 => bytes[at] = byte,
                1 => { bytes.remove(at); }
                _ => bytes.insert(at, byte),
            }
        }
        if let Ok(project) = from_json(&bytes) {
            prop_assert!(project.validate().is_ok());
        }
    }

    /// Replacing any one value in the document with an arbitrary JSON value never panics.
    #[test]
    fn serde_hostile_values_never_panic(
        pick in any::<prop::sample::Index>(),
        replacement in prop_oneof![
            Just(Value::Null),
            Just(json!(-1)),
            Just(json!(1e300)),
            Just(json!(18446744073709551615u64)),
            Just(json!("x")),
            Just(json!([])),
            Just(json!({})),
            Just(json!(true)),
            Just(json!(0.5)),
        ],
    ) {
        let mut doc: Value = serde_json::from_slice(&golden_v1()).unwrap();
        let mut paths = Vec::new();
        collect(&doc, &mut Vec::new(), &mut paths);
        let path = &paths[pick.index(paths.len())];
        let mut node = &mut doc;
        for step in path {
            node = match step {
                Step::Key(k) => node.get_mut(k.as_str()).unwrap(),
                Step::Index(i) => node.get_mut(*i).unwrap(),
            };
        }
        *node = replacement;
        let bytes = serde_json::to_vec(&doc).unwrap();
        if let Ok(project) = from_json(&bytes) {
            prop_assert!(project.validate().is_ok());
        }
    }
}

#[derive(Clone)]
enum Step {
    Key(String),
    Index(usize),
}

fn collect(value: &Value, path: &mut Vec<Step>, out: &mut Vec<Vec<Step>>) {
    out.push(path.clone());
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                path.push(Step::Key(key.clone()));
                collect(child, path, out);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                path.push(Step::Index(index));
                collect(child, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

#[test]
fn serde_truncated_files_never_panic() {
    let bytes = golden_v1();
    for len in 0..bytes.trim_ascii_end().len() {
        assert!(
            from_json(&bytes[..len]).is_err(),
            "prefix of {len} bytes loaded"
        );
    }
}

fn mutated(edit: impl FnOnce(&mut Value)) -> Result<Project, FormatError> {
    let mut doc: Value = serde_json::from_slice(&golden_v1()).unwrap();
    edit(&mut doc);
    from_json(&serde_json::to_vec(&doc).unwrap())
}

fn assert_invalid(result: Result<Project, FormatError>, needle: &str) {
    match result {
        Err(error) => {
            let text = error.to_string();
            assert!(
                text.contains(needle),
                "error {text:?} does not mention {needle:?}"
            );
        }
        Ok(_) => panic!("accepted a file that should break: {needle}"),
    }
}

#[test]
fn serde_rejects_unknown_and_malformed_versions() {
    assert!(matches!(from_json(b""), Err(FormatError::Json(_))));
    assert!(matches!(from_json(b"[]"), Err(FormatError::MissingVersion)));
    assert!(matches!(from_json(b"{}"), Err(FormatError::MissingVersion)));
    for version in [json!(0), json!(-1), json!("1"), json!(1.5), json!(null)] {
        let result = mutated(|d| d["format_version"] = version.clone());
        assert!(
            matches!(result, Err(FormatError::MissingVersion)),
            "{version}: {result:?}"
        );
    }
    let newer = mutated(|d| d["format_version"] = json!(FORMAT_VERSION + 1));
    assert!(matches!(newer, Err(FormatError::TooNew { .. })));
}

#[test]
fn serde_validates_time_signatures_and_tempo() {
    assert_invalid(
        mutated(|d| d["time_signature_map"][0]["denominator"] = json!(5)),
        "denominator",
    );
    assert_invalid(
        mutated(|d| d["time_signature_map"][0]["numerator"] = json!(0)),
        "numerator",
    );
    assert_invalid(
        mutated(|d| d["time_signature_map"][1]["bar"] = json!(0)),
        "later bar",
    );
    assert_invalid(
        mutated(|d| d["time_signature_map"] = json!([])),
        "at least one",
    );
    assert_invalid(
        mutated(|d| d["tempo_map"][0]["bpm"] = json!(0.5)),
        "beats per minute",
    );
    assert_invalid(
        mutated(|d| d["tempo_map"][1]["beat"] = json!(0.0)),
        "after the previous",
    );
    assert_invalid(mutated(|d| d["tempo_map"] = json!([])), "at least one");
}

#[test]
fn serde_validates_ids_and_references() {
    // Duplicate id within one collection.
    assert_invalid(
        mutated(|d| {
            let id = d["markers"][0]["id"].clone();
            d["clips"][0]["id"] = id;
        }),
        "more than one entity",
    );
    // Id at or above next_id.
    assert_invalid(mutated(|d| d["next_id"] = json!(2)), "next_id");
    // Clip on a missing track.
    assert_invalid(
        mutated(|d| d["clips"][0]["track"] = json!(9999)),
        "unknown track",
    );
    // Audio clip pointing at missing media.
    assert_invalid(
        mutated(|d| {
            for clip in d["clips"].as_array_mut().unwrap() {
                if clip["content"]["type"] == "audio" {
                    clip["content"]["media"] = json!(4242);
                }
            }
        }),
        "unknown media",
    );
    // A device no slot holds.
    assert_invalid(
        mutated(|d| d["buses"][0]["strip"]["inserts"] = json!([])),
        "in no slot",
    );
    // Insert chain naming a device that does not exist.
    assert_invalid(
        mutated(|d| d["master"]["inserts"] = json!([777])),
        "unknown device",
    );
    // A lane for a device that does not exist.
    assert_invalid(
        mutated(|d| d["automation"][0]["target"]["device"] = json!(31337)),
        "unknown device",
    );
}

#[test]
fn serde_validates_values_and_content() {
    assert_invalid(
        mutated(|d| d["tracks"][0]["strip"]["settings"]["gain_db"] = json!(1e9)),
        "gain_db",
    );
    assert_invalid(
        mutated(|d| d["tracks"][0]["strip"]["settings"]["pan"] = json!(2.0)),
        "pan",
    );
    assert_invalid(mutated(|d| d["clips"][0]["length"] = json!(0.0)), "length");
    assert_invalid(mutated(|d| d["clips"][0]["start"] = json!(-1.0)), "start");
    assert_invalid(mutated(|d| d["markers"][0]["beat"] = json!(1e300)), "beat");
    assert_invalid(
        mutated(|d| d["settings"]["sample_rate"] = json!(0)),
        "sample_rate",
    );
    assert_invalid(
        mutated(|d| d["tracks"][0]["color"] = json!(0x0100_0000)),
        "color",
    );
    assert_invalid(
        mutated(|d| d["tracks"][0]["name"] = json!("a\nb")),
        "control",
    );
    // A MIDI clip on an audio track and the reverse.
    assert_invalid(
        mutated(|d| {
            let audio_track = d["tracks"][0]["id"].clone();
            for clip in d["clips"].as_array_mut().unwrap() {
                if clip["content"]["type"] == "midi" {
                    clip["track"] = audio_track.clone();
                }
            }
        }),
        "MIDI clips belong on MIDI tracks",
    );
    // Instrument on an audio track.
    assert_invalid(
        mutated(|d| d["tracks"][0]["instrument"] = d["devices"][0]["id"].clone()),
        "instrument",
    );
    // Velocity zero.
    assert_invalid(
        mutated(|d| d["patterns"][0]["notes"][0]["velocity"] = json!(0)),
        "velocity",
    );
    // Unordered automation points.
    assert_invalid(
        mutated(|d| d["automation"][0]["points"][1]["beat"] = json!(-1.0)),
        "beat",
    );
}

#[test]
fn serde_rejects_media_paths_that_escape_the_bundle() {
    for path in [
        "../evil.wav",
        "audio/../../evil.wav",
        "/etc/passwd",
        "C:\\Windows\\x.wav",
        "audio\\..\\x.wav",
        "other/x.wav",
    ] {
        assert_invalid(
            mutated(|d| d["media_pool"][0]["path"] = json!(path)),
            "media.path",
        );
    }
}

#[test]
fn serde_rejects_bus_routing_cycles() {
    let mut p = Project::new("cycle");
    let a = BusId(1);
    let b = BusId(2);
    p.next_id = 3;
    for (id, to) in [(a, Output::Bus(b)), (b, Output::Bus(a))] {
        p.buses.push(Bus {
            id,
            name: "bus".into(),
            color: 0,
            strip: Strip {
                settings: StripSettings {
                    output: to,
                    ..StripSettings::default()
                },
                ..Strip::default()
            },
        });
    }
    assert!(p.validate().is_err());
    assert!(to_json(&p).is_err());
}

#[test]
fn serde_survives_hostile_shapes() {
    // Deep nesting hits serde_json's recursion limit instead of the stack.
    let deep = format!("{}1{}", "[".repeat(100_000), "]".repeat(100_000));
    assert!(from_json(deep.as_bytes()).is_err());
    // A long chain of buses (valid, linear) validates without recursion.
    let mut p = Project::new("chain");
    let count = 20_000_u64;
    p.next_id = count + 2;
    for i in 1..=count {
        let output = if i == count {
            Output::Master
        } else {
            Output::Bus(BusId(i + 1))
        };
        p.buses.push(Bus {
            id: BusId(i),
            name: String::new(),
            color: 0,
            strip: Strip {
                settings: StripSettings {
                    output,
                    ..StripSettings::default()
                },
                ..Strip::default()
            },
        });
    }
    p.validate().unwrap();
    // Closing the chain into a loop is detected.
    p.buses[count as usize - 1].strip.settings.output = Output::Bus(BusId(1));
    assert!(p.validate().is_err());
    // Out-of-range numbers are errors, not panics.
    for text in [
        r#"{"format_version":1,"name":"x","tempo_map":[{"beat":0,"bpm":1e999}],"time_signature_map":[],"next_id":1}"#,
        r#"{"format_version":1,"name":"x","tempo_map":[{"beat":0,"bpm":120}],"time_signature_map":[{"bar":0,"numerator":4,"denominator":4}],"next_id":-5}"#,
        r#"{"format_version":18446744073709551616}"#,
    ] {
        assert!(from_json(text.as_bytes()).is_err(), "{text}");
    }
}

fn synthetic_v0_to_v1(doc: &mut Value) -> Result<(), String> {
    // Version 0 (synthetic): `title` and a single `bpm` instead of `name` and a tempo map.
    let object = doc.as_object_mut().ok_or("not an object")?;
    let title = object.remove("title").ok_or("no title")?;
    let bpm = object.remove("bpm").ok_or("no bpm")?;
    let mut upgraded = serde_json::to_value(Project::new("")).map_err(|e| e.to_string())?;
    upgraded["name"] = title;
    upgraded["tempo_map"] = json!([{ "beat": 0.0, "bpm": bpm, "ramp": false }]);
    let version = object.get("format_version").cloned();
    *doc = upgraded;
    if let Some(version) = version {
        doc["format_version"] = version;
    }
    Ok(())
}

#[test]
fn serde_migration_chain_upgrades_a_synthetic_v0_file() {
    let chain: &[Migration] = &[synthetic_v0_to_v1];
    let mut doc = json!({ "format_version": 0, "title": "Old song", "bpm": 90.0 });
    migrate_chain(&mut doc, 0, 1, 0, chain).unwrap();
    assert_eq!(doc["format_version"], json!(1));
    let project: Project = serde_json::from_value(doc).unwrap();
    project.validate().unwrap();
    assert_eq!(project.name, "Old song");
    assert_eq!(project.tempo_map.bpm_at(0.0), 90.0);

    // A step that fails, and a chain that is missing a step, are reported, not skipped.
    let mut broken = json!({ "format_version": 0 });
    assert!(matches!(
        migrate_chain(&mut broken, 0, 1, 0, chain),
        Err(FormatError::Migration(_))
    ));
    let mut gap = json!({ "format_version": 0 });
    assert!(matches!(
        migrate_chain(&mut gap, 0, 2, 0, chain),
        Err(FormatError::Migration(_))
    ));
    // Multi-step chains run in order and stamp each version.
    let bump: Migration = |doc| {
        doc["steps"] = json!(doc["steps"].as_u64().unwrap_or(0) + 1);
        Ok(())
    };
    let mut doc = json!({ "format_version": 1 });
    migrate_chain(&mut doc, 1, 4, 1, &[bump, bump, bump]).unwrap();
    assert_eq!(doc["format_version"], json!(4));
    assert_eq!(doc["steps"], json!(3));
}

#[test]
fn serde_current_version_has_a_golden_file() {
    // Bumping FORMAT_VERSION without committing tests/fixtures/vN/project.json fails here.
    for version in 1..=FORMAT_VERSION {
        assert!(
            golden_path(version).exists(),
            "missing golden file for v{version}"
        );
        from_json(&std::fs::read(golden_path(version)).unwrap()).unwrap();
    }
}

/// An arrangement edited the way stage 13 edits it (trims, a split, fades, clip gain and mute,
/// a split MIDI clip) saves and reloads identically.
#[test]
fn serde_edited_arrangement_round_trips() {
    let mut ed = Editor::new(Project::new("Edited"));
    let media: MediaId = ed.alloc_id().unwrap();
    ed.apply(EditCommand::AddMedia {
        item: MediaItem {
            id: media,
            name: "Loop".into(),
            path: "audio/loop.wav".into(),
            hash: "abc123".into(),
            sample_rate: 44_100,
            channels: 2,
            frames: 441_000,
        },
    })
    .unwrap();
    let track = |ed: &mut Editor, kind: TrackKind| -> TrackId {
        let id: TrackId = ed.alloc_id().unwrap();
        ed.apply(EditCommand::InsertTrack {
            index: ed.project().tracks.len(),
            bundle: Box::new(Owned::new(Track {
                id,
                name: "t".into(),
                color: 0x4aa3ff,
                kind,
                strip: Strip::default(),
                instrument: None,
            })),
        })
        .unwrap();
        id
    };
    let audio_track = track(&mut ed, TrackKind::Audio);
    let midi_track = track(&mut ed, TrackKind::Midi);
    let audio = |id: ClipId, start: f64, length: f64, offset: u64| Clip {
        id,
        track: audio_track,
        name: "loop".into(),
        start,
        length,
        muted: false,
        content: ClipContent::Audio {
            media,
            source_offset: offset,
            gain_db: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
        },
    };
    let whole: ClipId = ed.alloc_id().unwrap();
    ed.apply(EditCommand::AddClip {
        clip: Box::new(audio(whole, 4.0, 16.0, 0)),
    })
    .unwrap();
    // Left trim by 2 beats (1 s at 120 BPM = 44100 frames), right trim by 2 beats, fades,
    // gain and mute, as ReplaceClip commands.
    let mut trimmed = audio(whole, 6.0, 12.0, 44_100);
    if let ClipContent::Audio {
        gain_db,
        fade_in,
        fade_out,
        ..
    } = &mut trimmed.content
    {
        *gain_db = -6.5;
        *fade_in = 1.25;
        *fade_out = 0.5;
    }
    ed.apply(EditCommand::ReplaceClip {
        clip: Box::new(trimmed.clone()),
    })
    .unwrap();
    // Split at beat 12: the left part keeps the id, the right part gets a fresh one.
    let right: ClipId = ed.alloc_id().unwrap();
    let mut left = trimmed.clone();
    left.length = 6.0;
    if let ClipContent::Audio { fade_out, .. } = &mut left.content {
        *fade_out = 0.0;
    }
    let mut second = audio(right, 12.0, 6.0, 44_100 + 6 * 22_050);
    second.muted = true;
    if let ClipContent::Audio {
        gain_db, fade_out, ..
    } = &mut second.content
    {
        *gain_db = -6.5;
        *fade_out = 0.5;
    }
    ed.apply(EditCommand::ReplaceClip {
        clip: Box::new(left),
    })
    .unwrap();
    ed.apply(EditCommand::AddClip {
        clip: Box::new(second),
    })
    .unwrap();
    // A MIDI clip split at beat 2: notes after the split move to the right clip, rebased.
    let note = |start, key| Note {
        start,
        length: 1.0,
        key,
        velocity: 100,
    };
    let midi = |id: ClipId, start: f64, length: f64, notes: Vec<Note>| Clip {
        id,
        track: midi_track,
        name: "riff".into(),
        start,
        length,
        muted: false,
        content: ClipContent::Midi {
            notes,
            controllers: vec![],
        },
    };
    let riff: ClipId = ed.alloc_id().unwrap();
    ed.apply(EditCommand::AddClip {
        clip: Box::new(midi(riff, 0.0, 4.0, vec![note(0.0, 60), note(2.5, 64)])),
    })
    .unwrap();
    let riff_right: ClipId = ed.alloc_id().unwrap();
    ed.apply(EditCommand::ReplaceClip {
        clip: Box::new(midi(riff, 0.0, 2.0, vec![note(0.0, 60)])),
    })
    .unwrap();
    ed.apply(EditCommand::AddClip {
        clip: Box::new(midi(riff_right, 2.0, 2.0, vec![note(0.5, 64)])),
    })
    .unwrap();

    let project = ed.project().clone();
    let bytes = to_json(&project).unwrap();
    let loaded = from_json(&bytes).unwrap();
    assert_eq!(loaded, project);
    // Writing the loaded project again is byte-identical.
    assert_eq!(to_json(&loaded).unwrap(), bytes);
    assert_eq!(loaded.clips.len(), 4);
    let clip = &loaded.clips[&right];
    assert!(clip.muted);
    assert_eq!(clip.start, 12.0);
    assert!(matches!(
        clip.content,
        ClipContent::Audio { source_offset: 176_400, gain_db, fade_out, .. }
            if gain_db == -6.5 && fade_out == 0.5
    ));
}
