//! A plugin that reports an absurd latency (`u32::MAX` frames) is held to ten seconds, and delay
//! compensation and export stay sane. This test is a binary of its own because it sets an
//! environment variable the fixture plugins read.

#![allow(unsafe_code)]

mod common;

use std::collections::BTreeMap;

use ow_engine::export::{ExportError, MAX_EXPORT_FRAMES, render_project};
use ow_engine::{CompileContext, MAX_BLOCK, compile_project};
use ow_model::{Editor, Project};
use ow_plugin_fixtures::LATENCY_ENV;

use common::*;

#[test]
fn a_plugin_reporting_u32_max_latency_is_held_to_ten_seconds() {
    // SAFETY: set before any plugin is loaded or any other thread exists in this test binary.
    unsafe { std::env::set_var(LATENCY_ENV, u32::MAX.to_string()) };
    let cache = fixture_cache();
    let ten_seconds = RATE * 10;

    for kind in [gain_kind(), vst3_gain_kind()] {
        // The scanner (a child that inherited the variable) already held it to the limit.
        let scanned = cache.directory().find(&kind).unwrap();
        assert!(
            scanned.latency <= ten_seconds,
            "{kind}: {}",
            scanned.latency
        );
        let plugin = cache
            .acquire(1, &kind, &BTreeMap::new(), None, RATE, BLOCK)
            .unwrap();
        assert_eq!(plugin.latency_frames(), ten_seconds, "{kind}");
        assert_eq!(plugin.processor().latency_frames(), ten_seconds, "{kind}");
        cache.clear();
    }

    // Delay compensation takes the clamped value: no overflow, no absurd delay lines.
    let mut editor = Editor::new(Project::new("latency"));
    add_track(
        &mut editor,
        (&tone_kind(), &[]),
        &[(&vst3_gain_kind(), &[]), (&gain_kind(), &[])],
    );
    let project = editor.project().clone();
    let ctx = CompileContext::default().with_plugins(cache.clone());
    let compiled = compile_project(&project, &ctx, RATE, MAX_BLOCK).unwrap();
    assert_eq!(compiled.plan.output_latency(), 2 * ten_seconds);

    // The latency is rendered on top of the export and counts against its limit.
    let frames = MAX_EXPORT_FRAMES - 1000;
    assert!(matches!(
        render_project(&project, &ctx, frames),
        Err(ExportError::TooLong { .. })
    ));
}
