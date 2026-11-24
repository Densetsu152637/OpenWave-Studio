//! The `.owave` bundle: autosave rotation and `.owz` archive import and export.

mod common;

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use common::reference_project;
use ow_model::bundle::{self, DEFAULT_BACKUP_LIMIT};
use ow_model::owz;
use ow_model::*;
use zip::write::SimpleFileOptions;

fn at(seconds: u64) -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_780_000_000 + seconds)
}

#[test]
fn bundle_save_and_load_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let bundle_dir = dir.path().join("Song.owave");
    let project = reference_project();
    bundle::save(&bundle_dir, &project).unwrap();
    assert_eq!(bundle::load(&bundle_dir).unwrap(), project);
}

#[test]
fn autosave_keeps_a_bounded_number_of_backups() {
    let dir = tempfile::tempdir().unwrap();
    let mut project = reference_project();
    bundle::save(dir.path(), &project).unwrap();
    // An unrelated file in backups/ is never touched.
    fs::write(dir.path().join("backups/notes.txt"), "keep me").unwrap();
    let mut written = Vec::new();
    for i in 0..30_u64 {
        project.name = format!("version {i}");
        written.push(bundle::autosave(dir.path(), &project, at(i * 60), 5).unwrap());
        assert!(bundle::list_backups(dir.path()).unwrap().len() <= 5);
    }
    let kept = bundle::list_backups(dir.path()).unwrap();
    assert_eq!(kept.len(), 5);
    // Newest first, and exactly the last five written.
    let expected: Vec<_> = written.iter().rev().take(5).cloned().collect();
    assert_eq!(kept, expected);
    assert_eq!(bundle::load_file(&kept[0]).unwrap().name, "version 29");
    assert_eq!(bundle::load_file(&kept[4]).unwrap().name, "version 25");
    assert!(dir.path().join("backups/notes.txt").exists());
    const { assert!(DEFAULT_BACKUP_LIMIT >= 1) };
}

#[test]
fn autosave_in_the_same_second_gets_a_new_name() {
    let dir = tempfile::tempdir().unwrap();
    let project = reference_project();
    let first = bundle::autosave(dir.path(), &project, at(0), 20).unwrap();
    let second = bundle::autosave(dir.path(), &project, at(0), 20).unwrap();
    assert_ne!(first, second);
    assert_eq!(
        bundle::list_backups(dir.path()).unwrap(),
        vec![second, first]
    );
}

#[test]
fn autosave_names_are_sortable_utc_stamps() {
    let dir = tempfile::tempdir().unwrap();
    let path = bundle::autosave(
        dir.path(),
        &reference_project(),
        UNIX_EPOCH + Duration::from_secs(951_782_400 + 3_723),
        3,
    )
    .unwrap();
    assert_eq!(path.file_name().unwrap(), "project-20000229-010203.json");
}

fn bundle_with_media(root: &Path) -> std::path::PathBuf {
    let dir = root.join("Song.owave");
    bundle::save(&dir, &reference_project()).unwrap();
    fs::write(dir.join("audio/3fa9c0de.wav"), vec![7_u8; 10_000]).unwrap();
    fs::create_dir_all(dir.join("audio/takes")).unwrap();
    fs::write(dir.join("audio/takes/one.wav"), b"RIFFtake").unwrap();
    fs::write(dir.join("cache/peaks.owpk"), b"cache").unwrap();
    bundle::autosave(&dir, &reference_project(), at(0), 5).unwrap();
    dir
}

#[test]
fn owz_export_and_import_round_trip() {
    let root = tempfile::tempdir().unwrap();
    let dir = bundle_with_media(root.path());
    let archive = root.path().join("Song.owz");
    owz::export(&dir, &archive).unwrap();

    let names = {
        let mut zip = zip::ZipArchive::new(fs::File::open(&archive).unwrap()).unwrap();
        let mut names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().unwrap().into_owned())
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        names,
        ["audio/3fa9c0de.wav", "audio/takes/one.wav", "project.json"],
        "cache and backups are not archived"
    );

    let out = root.path().join("Copy.owave");
    let project = owz::import(&archive, &out).unwrap();
    assert_eq!(project, reference_project());
    assert_eq!(bundle::load(&out).unwrap(), project);
    assert_eq!(
        fs::read(out.join("audio/3fa9c0de.wav")).unwrap(),
        vec![7_u8; 10_000]
    );
    assert_eq!(
        fs::read(out.join("audio/takes/one.wav")).unwrap(),
        b"RIFFtake"
    );
    for sub in bundle::BUNDLE_DIRS {
        assert!(out.join(sub).is_dir());
    }
    assert!(!out.join("cache/peaks.owpk").exists());
}

#[test]
fn owz_import_into_an_empty_existing_directory_and_refuses_a_full_one() {
    let root = tempfile::tempdir().unwrap();
    let dir = bundle_with_media(root.path());
    let archive = root.path().join("Song.owz");
    owz::export(&dir, &archive).unwrap();

    let empty = root.path().join("empty");
    fs::create_dir(&empty).unwrap();
    owz::import(&archive, &empty).unwrap();

    let full = root.path().join("full");
    fs::create_dir(&full).unwrap();
    fs::write(full.join("mine.txt"), "x").unwrap();
    assert!(matches!(
        owz::import(&archive, &full),
        Err(BundleError::DestinationExists(_))
    ));
    assert_eq!(fs::read_to_string(full.join("mine.txt")).unwrap(), "x");
}

fn archive_with(path: &Path, entries: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, data) in entries {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn owz_import_rejects_hostile_archives_and_cleans_up() {
    let root = tempfile::tempdir().unwrap();
    let good = format::to_json(&reference_project()).unwrap();
    type Entries<'a> = Vec<(&'a str, &'a [u8])>;
    let cases: Vec<(&str, Entries)> = vec![
        (
            "zip slip",
            vec![("project.json", &good), ("../evil.txt", b"x")],
        ),
        (
            "zip slip in audio",
            vec![("project.json", &good), ("audio/../../evil.txt", b"x")],
        ),
        (
            "absolute path",
            vec![("project.json", &good), ("/tmp/evil.txt", b"x")],
        ),
        (
            "backslash",
            vec![("project.json", &good), ("audio\\evil.wav", b"x")],
        ),
        (
            "unexpected file",
            vec![("project.json", &good), ("run.exe", b"x")],
        ),
        ("no project", vec![("audio/a.wav", b"x")]),
        (
            "invalid project",
            vec![("project.json", b"{\"format_version\":1}")],
        ),
        (
            "newer project",
            vec![("project.json", b"{\"format_version\":99}")],
        ),
        ("not json", vec![("project.json", b"MZ\x90\x00")]),
    ];
    for (label, entries) in cases {
        let archive = root.path().join("bad.owz");
        archive_with(&archive, &entries);
        let out = root.path().join("out.owave");
        let result = owz::import(&archive, &out);
        assert!(result.is_err(), "{label} was accepted");
        assert!(!out.exists(), "{label}: destination left behind");
        assert!(
            !root.path().join("evil.txt").exists(),
            "{label}: escaped the destination"
        );
    }
    // Not a zip at all.
    let junk = root.path().join("junk.owz");
    fs::write(&junk, b"this is not a zip").unwrap();
    assert!(owz::import(&junk, &root.path().join("o2")).is_err());
    assert!(!root.path().join("o2").exists());
}

#[test]
fn owz_export_is_atomic_and_refuses_an_invalid_bundle() {
    let root = tempfile::tempdir().unwrap();
    let empty = root.path().join("nothing.owave");
    fs::create_dir(&empty).unwrap();
    let archive = root.path().join("x.owz");
    fs::write(&archive, b"previous archive").unwrap();
    assert!(owz::export(&empty, &archive).is_err());
    assert_eq!(fs::read(&archive).unwrap(), b"previous archive");
    assert!(!root.path().join("x.owz.tmp").exists());
}
