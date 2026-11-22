//! Saves are atomic: an interrupted write leaves the previous `project.json` intact.

mod common;

use std::fs;
use std::io;

use common::reference_project;
use ow_model::bundle::{self, AtomicStage, PROJECT_FILE, write_atomic, write_atomic_with};
use ow_model::*;

fn named(name: &str) -> Project {
    let mut p = reference_project();
    p.name = name.into();
    p
}

#[test]
fn atomic_save_replaces_the_file_and_leaves_no_temp() {
    let dir = tempfile::tempdir().unwrap();
    bundle::save(dir.path(), &named("one")).unwrap();
    bundle::save(dir.path(), &named("two")).unwrap();
    assert_eq!(bundle::load(dir.path()).unwrap().name, "two");
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    for sub in bundle::BUNDLE_DIRS {
        assert!(dir.path().join(sub).is_dir());
    }
}

#[test]
fn atomic_save_interrupted_write_keeps_the_previous_file() {
    for stage in [AtomicStage::TempWritten, AtomicStage::TempSynced] {
        let dir = tempfile::tempdir().unwrap();
        bundle::save(dir.path(), &named("previous")).unwrap();
        let path = dir.path().join(PROJECT_FILE);
        let before = fs::read(&path).unwrap();

        let new = format::to_json(&named("new")).unwrap();
        let result = write_atomic_with(&path, &new, &mut |at| {
            if at == stage {
                Err(io::Error::other("simulated crash"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), before, "{stage:?}");
        assert_eq!(bundle::load(dir.path()).unwrap().name, "previous");
        assert!(
            !dir.path().join("project.json.tmp").exists(),
            "temp file cleaned up"
        );
    }
}

#[test]
fn atomic_save_survives_a_crash_that_leaves_a_partial_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    bundle::save(dir.path(), &named("previous")).unwrap();
    // What a process killed mid-write leaves behind: half a document next to the real file.
    let full = format::to_json(&named("new")).unwrap();
    fs::write(dir.path().join("project.json.tmp"), &full[..full.len() / 2]).unwrap();
    assert_eq!(bundle::load(dir.path()).unwrap().name, "previous");
    // The next save overwrites the stale temp file and completes.
    bundle::save(dir.path(), &named("after")).unwrap();
    assert_eq!(bundle::load(dir.path()).unwrap().name, "after");
    assert!(!dir.path().join("project.json.tmp").exists());
}

#[test]
fn atomic_save_refuses_an_invalid_project_and_keeps_the_file() {
    let dir = tempfile::tempdir().unwrap();
    bundle::save(dir.path(), &named("good")).unwrap();
    let mut broken = named("broken");
    broken.settings.sample_rate = 0;
    assert!(matches!(
        bundle::save(dir.path(), &broken),
        Err(BundleError::Format(FormatError::Invalid(_)))
    ));
    assert_eq!(bundle::load(dir.path()).unwrap().name, "good");
}

#[test]
fn atomic_save_writes_exactly_the_bytes_given() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.bin");
    for len in [0_usize, 1, 4096, 1 << 20] {
        let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        write_atomic(&path, &data).unwrap();
        assert_eq!(fs::read(&path).unwrap(), data);
    }
}

#[test]
fn atomic_save_into_a_missing_directory_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing").join(PROJECT_FILE);
    assert!(write_atomic(&path, b"x").is_err());
}
