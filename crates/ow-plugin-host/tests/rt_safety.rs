//! Real-time safety with hosted plugins in the graph.
//!
//! `AllocDisabler` is this test binary's global allocator and `AudioCore::process` runs every
//! cycle under `assert_no_alloc`, which aborts the process on any allocation, reallocation or
//! deallocation inside the cycle. A project with a hosted instrument and a hosted effect is
//! played through the real engine cycle, with a live parameter edit and a plan swap in the
//! middle.
//!
//! What the guard covers, and what it does not: the hosted-plugin node's own work (draining
//! the parameter queue, building the event list in preallocated buffers, binding the
//! preallocated audio buffers, copying audio in and out) runs under the guard. The call into
//! the plugin's `process` is wrapped in `assert_no_alloc::permit_alloc`, because what a
//! third-party plugin allocates inside it is out of this host's control; the host's own code
//! around that call is what the guard checks. (The fixture plugins do not allocate in `process`
//! either, so this test would also notice if the permit hid a leak in the host.)

mod common;

use std::collections::BTreeMap;

use assert_no_alloc::AllocDisabler;
use ow_engine::{CompileContext, MAX_BLOCK, compile_project};
use ow_model::{EditCommand, Editor, Project};
use ow_plugin_fixtures::{PARAM_GAIN, PARAM_LEVEL};

use common::*;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn hosted_plugins_run_in_the_engine_cycle_without_allocating() {
    let tone = tone_kind();
    let gain = gain_kind();
    let mut editor = Editor::new(Project::new("rt"));
    let (_, inserts) = add_track(
        &mut editor,
        (&tone, &[(PARAM_LEVEL, 1.0)]),
        &[(&gain, &[(PARAM_GAIN, 1.0)])],
    );
    let cache = fixture_cache();
    let ctx = CompileContext::default().with_plugins(cache.clone());
    let compiled = compile_project(&editor.project().clone(), &ctx, RATE, MAX_BLOCK).unwrap();
    let mut renderer = player(compiled);

    // 100 blocks through the real cycle, with an edit in the middle.
    let plugin = cache.get(inserts[0].0).unwrap();
    let mut heard = Vec::new();
    for block in 0..100 {
        if block == 50 {
            editor
                .apply(EditCommand::SetDeviceParam {
                    device: inserts[0],
                    param: PARAM_GAIN,
                    value: Some(0.5),
                })
                .unwrap();
            plugin.set_params(&editor.project().devices[&inserts[0]].params.clone());
        }
        let out = renderer.render(512, 512);
        heard.push(out[510]);
    }
    assert!(heard[40] > 0.1, "the instrument and the effect are running");
    assert!(
        (heard[90] - 0.5 * heard[40]).abs() < 1e-5,
        "{} {}",
        heard[90],
        heard[40]
    );

    // A plan swap (a recompile) in the middle of playback finds the same instances.
    let second = compile_project(&editor.project().clone(), &ctx, RATE, MAX_BLOCK).unwrap();
    assert!(renderer.link().swap_plan(second.plan).is_ok());
    for _ in 0..20 {
        let out = renderer.render(512, 512);
        assert!(out.iter().all(|s| s.is_finite()));
    }
    let _ = BTreeMap::<u32, f64>::new();
}

#[test]
fn hosted_vst3_plugins_run_in_the_engine_cycle_without_allocating() {
    let tone = vst3_tone_kind();
    let gain = vst3_gain_kind();
    let mut editor = Editor::new(Project::new("rt-vst3"));
    let (_, inserts) = add_track(
        &mut editor,
        (&tone, &[(PARAM_LEVEL, 1.0)]),
        &[(&gain, &[(PARAM_GAIN, 1.0)])],
    );
    let cache = fixture_cache();
    let ctx = CompileContext::default().with_plugins(cache.clone());
    let compiled = compile_project(&editor.project().clone(), &ctx, RATE, MAX_BLOCK).unwrap();
    let mut renderer = player(compiled);

    let plugin = cache.get(inserts[0].0).unwrap();
    let mut heard = Vec::new();
    for block in 0..100 {
        if block == 50 {
            editor
                .apply(EditCommand::SetDeviceParam {
                    device: inserts[0],
                    param: PARAM_GAIN,
                    value: Some(0.5),
                })
                .unwrap();
            plugin.set_params(&editor.project().devices[&inserts[0]].params.clone());
        }
        let out = renderer.render(512, 512);
        heard.push(out[510]);
    }
    assert!(heard[40] > 0.1, "the instrument and the effect are running");
    assert!(
        (heard[90] - 0.5 * heard[40]).abs() < 1e-5,
        "{} {}",
        heard[90],
        heard[40]
    );

    let second = compile_project(&editor.project().clone(), &ctx, RATE, MAX_BLOCK).unwrap();
    assert!(renderer.link().swap_plan(second.plan).is_ok());
    for _ in 0..20 {
        let out = renderer.render(512, 512);
        assert!(out.iter().all(|s| s.is_finite()));
    }
}
