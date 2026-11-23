//! Regenerates the committed UI artifacts. CI runs
//! `cargo test -p ow-ipc export_bindings && git diff --exit-code src/lib/ipc/generated
//! src/lib/stores/fixtures`, so a changed DTO that was not regenerated fails the build.
//!
//! * `export_bindings`: the `ts-rs` TypeScript types into `src/lib/ipc/generated/`.
//! * `export_bindings_stream_fixture`: a deterministic snapshot plus patch stream produced by a
//!   real [`Session`], which the TypeScript store tests replay (`src/lib/stores/fixtures/`).

mod common;

use std::path::Path;

use common::{action, run};
use ow_ipc::{
    AnalysisReport, AnalysisRequest, CommandReply, DeviceDescriptor, ExportOptions, ExportResult,
    HistoryState, MediaImportReply, ModelPatch, PatchOp, Project, Session, Snapshot, UiCommand,
};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
use serde::Serialize;
use ts_rs::{Config, TS};

/// Where the generated types go, relative to the workspace root.
const GENERATED: &str = "../../src/lib/ipc/generated";

#[test]
fn export_bindings() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(GENERATED);
    // Start from an empty directory so a type that was renamed or removed cannot leave a stale
    // file behind (CI then sees the deletion in `git diff`).
    if dir.is_dir() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "ts") {
                std::fs::remove_file(path).unwrap();
            }
        }
    }
    let cfg = Config::from_env().with_out_dir(dir);
    UiCommand::export_all(&cfg).unwrap();
    PatchOp::export_all(&cfg).unwrap();
    ModelPatch::export_all(&cfg).unwrap();
    Snapshot::export_all(&cfg).unwrap();
    HistoryState::export_all(&cfg).unwrap();
    CommandReply::export_all(&cfg).unwrap();
    MediaImportReply::export_all(&cfg).unwrap();
    DeviceDescriptor::export_all(&cfg).unwrap();
    AnalysisRequest::export_all(&cfg).unwrap();
    AnalysisReport::export_all(&cfg).unwrap();
    ExportOptions::export_all(&cfg).unwrap();
    ExportResult::export_all(&cfg).unwrap();
}

#[derive(Serialize)]
struct Fixture {
    /// State before the first patch.
    initial: Snapshot,
    /// Every patch, in order (numbered from 1).
    patches: Vec<ModelPatch>,
    /// The session state after patch `seq`, for every `seq` that is a multiple of 8 (what a
    /// resync request would have answered at that moment).
    checkpoints: Vec<Snapshot>,
    /// State after the last patch.
    last: Snapshot,
}

#[test]
fn export_bindings_stream_fixture() {
    let mut runner = TestRunner::deterministic();
    let actions = proptest::collection::vec(action(), 130)
        .new_tree(&mut runner)
        .unwrap()
        .current();
    let mut session = Session::new(Project::new("Fixture"));
    let initial = session.snapshot();
    let mut patches = Vec::new();
    let mut checkpoints = Vec::new();
    for action in &actions {
        run(&mut session, action, |live, patch| {
            if patch.seq % 8 == 0 {
                checkpoints.push(live.snapshot());
            }
            patches.push(patch);
        });
    }
    assert!(patches.len() >= 40, "only {} patches", patches.len());
    let fixture = Fixture {
        initial,
        patches,
        checkpoints,
        last: session.snapshot(),
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../src/lib/stores/fixtures/mirror-stream.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut json = serde_json::to_string_pretty(&fixture).unwrap();
    json.push('\n');
    std::fs::write(path, json).unwrap();
}

#[test]
fn export_bindings_ui_command_shapes() {
    // Pin the wire shape of the commands the UI sends.
    let json = serde_json::to_value(UiCommand::Begin {
        label: "Drag".into(),
    })
    .unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "type": "begin", "label": "Drag" })
    );
    let json = serde_json::to_value(UiCommand::Edit {
        edit: ow_ipc::EditCommand::SetName { name: "x".into() },
    })
    .unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "type": "edit", "edit": { "command": "set_name", "name": "x" } })
    );
    let parsed: UiCommand = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, UiCommand::Edit { .. }));
}
