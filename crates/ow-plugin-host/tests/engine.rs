//! Hosted plugins inside the engine: compiling `clap:<id>` devices into graph nodes, delay
//! compensation, instances that survive recompiles, and live parameter edits without one.

mod common;

use std::collections::BTreeMap;

use ow_engine::{CompileContext, MAX_BLOCK, NodeKey, compile_project};
use ow_model::{EditCommand, Editor, Project};
use ow_plugin_fixtures::{GAIN_LATENCY, PARAM_GAIN, PARAM_LEVEL};

use common::*;

#[test]
fn plugin_latency_reaches_delay_compensation() {
    let tone = tone_kind();
    let gain = gain_kind();
    let mut editor = Editor::new(Project::new("pdc"));
    // Track A runs through the gain plugin (64 frames of latency); track B does not.
    let (_, inserts) = add_track(&mut editor, (&tone, &[]), &[(&gain, &[])]);
    add_track(&mut editor, (&tone, &[]), &[]);
    let project = editor.project().clone();
    let ctx = CompileContext::default().with_plugins(fixture_cache());
    let compiled = compile_project(&project, &ctx, RATE, MAX_BLOCK).unwrap();

    // The compiler read the plugin's latency into the plan: the mix is 64 frames late and the
    // plugin's own node reports it.
    assert_eq!(compiled.plan.output_latency(), GAIN_LATENCY);
    let node = compiled.nodes.get(NodeKey::Device(inserts[0])).unwrap();
    assert_eq!(compiled.plan.latency_of(node), Some(GAIN_LATENCY));

    // Both tracks start sounding at the same frame: the track without the plugin was delayed to
    // match. Without compensation B would already sound in the first 64 frames.
    let mut renderer = player(compiled);
    let out = render(&mut renderer, 2048);
    let lag = GAIN_LATENCY as usize;
    assert!(
        out[..lag * 2].iter().all(|s| *s == 0.0),
        "nothing may sound before the latency has passed"
    );
    assert!(out[lag * 2 + 40..].iter().any(|s| s.abs() > 0.1));
    // After the latency both tracks contribute equally (same level, same pan): the level is
    // exactly twice one track's.
    let both = out[1500 * 2];
    let mut one = Editor::new(Project::new("one"));
    add_track(&mut one, (&tone, &[]), &[]);
    let single = compile_project(&one.project().clone(), &ctx, RATE, MAX_BLOCK).unwrap();
    let mut single = player(single);
    let alone = render(&mut single, 2048)[1500 * 2];
    assert!((both - 2.0 * alone).abs() < 1e-6, "{both} vs {alone}");
}

#[test]
fn a_recompile_reuses_the_plugin_instance_and_edits_need_no_recompile() {
    let tone = tone_kind();
    let gain = gain_kind();
    let mut editor = Editor::new(Project::new("survive"));
    let (_, inserts) = add_track(
        &mut editor,
        (&tone, &[(PARAM_LEVEL, 1.0)]),
        &[(&gain, &[(PARAM_GAIN, 1.0)])],
    );
    let cache = fixture_cache();
    let ctx = CompileContext::default().with_plugins(cache.clone());
    let before = cache.instances_created();
    let first = compile_project(&editor.project().clone(), &ctx, RATE, MAX_BLOCK).unwrap();
    // Two plugins (tone and gain) were instantiated once.
    assert_eq!(cache.instances_created(), before + 2);
    let plugin = cache.get(inserts[0].0).unwrap();

    let mut renderer = player(first);
    let loud = render(&mut renderer, 4096);
    let level = loud[3000 * 2];
    assert!(level > 0.1, "{level}");

    // A parameter edit: the control layer updates the model and queues the value on the plugin
    // instance; the running plan changes without being replaced.
    editor
        .apply(EditCommand::SetDeviceParam {
            device: inserts[0],
            param: PARAM_GAIN,
            value: Some(0.5),
        })
        .unwrap();
    let changed = editor.project().devices[&inserts[0]].params.clone();
    assert_eq!(plugin.set_params(&changed), 1);
    let quiet = render(&mut renderer, 2048);
    let half = quiet[1500 * 2];
    assert!((half - 0.5 * level).abs() < 1e-5, "{half} vs {level}");
    assert_eq!(cache.instances_created(), before + 2);

    // A full recompile (any structural edit) finds the same instances and keeps the value.
    let second = compile_project(&editor.project().clone(), &ctx, RATE, MAX_BLOCK).unwrap();
    assert_eq!(cache.instances_created(), before + 2);
    assert!(std::sync::Arc::ptr_eq(
        &plugin,
        &cache.get(inserts[0].0).unwrap()
    ));
    let mut renderer = player(second);
    let again = render(&mut renderer, 4096);
    assert!((again[3000 * 2] - 0.5 * level).abs() < 1e-5);

    // Removing the track releases its plugins at the next compile.
    let removed = Editor::new(Project::new("empty")).project().clone();
    compile_project(&removed, &ctx, RATE, MAX_BLOCK).unwrap();
    assert!(cache.is_empty());
}

#[test]
fn a_plugin_that_is_not_installed_compiles_as_unknown() {
    let missing = "clap:com.not.installed";
    let mut editor = Editor::new(Project::new("missing"));
    // A missing instrument is silent; a missing insert is a pass-through.
    add_track(&mut editor, (&tone_kind(), &[]), &[(missing, &[])]);
    add_track(&mut editor, (missing, &[]), &[]);
    let ctx = CompileContext::default().with_plugins(fixture_cache());
    let compiled = compile_project(&editor.project().clone(), &ctx, RATE, MAX_BLOCK).unwrap();
    assert_eq!(compiled.plan.output_latency(), 0);
    let mut renderer = player(compiled);
    let out = render(&mut renderer, 2048);
    assert!(out[1500 * 2].abs() > 0.1, "the first track still sounds");

    // Without any plugin cache a plugin device is just an unknown kind too.
    let bare = compile_project(
        &editor.project().clone(),
        &CompileContext::default(),
        RATE,
        MAX_BLOCK,
    )
    .unwrap();
    let mut renderer = player(bare);
    let out = render(&mut renderer, 2048);
    assert!(out.iter().all(|s| *s == 0.0));
    let _ = BTreeMap::<u32, f64>::new();
}

#[test]
fn vst3_plugins_feed_delay_compensation_and_survive_recompiles() {
    let tone = vst3_tone_kind();
    let gain = vst3_gain_kind();
    let mut editor = Editor::new(Project::new("vst3"));
    let (_, inserts) = add_track(
        &mut editor,
        (&tone, &[(PARAM_LEVEL, 1.0)]),
        &[(&gain, &[(PARAM_GAIN, 1.0)])],
    );
    add_track(&mut editor, (&tone, &[(PARAM_LEVEL, 1.0)]), &[]);
    let cache = fixture_cache();
    let ctx = CompileContext::default().with_plugins(cache.clone());
    let before = cache.instances_created();
    let first = compile_project(&editor.project().clone(), &ctx, RATE, MAX_BLOCK).unwrap();
    // Three VST3 instances (two tones and a gain), each created once.
    assert_eq!(cache.instances_created(), before + 3);

    // The gain's 64 frames of latency reach the plan; the track without it is delayed to match.
    assert_eq!(first.plan.output_latency(), GAIN_LATENCY);
    let node = first.nodes.get(NodeKey::Device(inserts[0])).unwrap();
    assert_eq!(first.plan.latency_of(node), Some(GAIN_LATENCY));
    let mut renderer = player(first);
    let out = render(&mut renderer, 4096);
    let lag = GAIN_LATENCY as usize;
    assert!(out[..lag * 2].iter().all(|s| *s == 0.0));
    let both = out[3000 * 2];
    assert!(both > 0.1, "{both}");

    // A live edit reaches the instance without a recompile: the gain halves one of two tracks.
    let plugin = cache.get(inserts[0].0).unwrap();
    editor
        .apply(EditCommand::SetDeviceParam {
            device: inserts[0],
            param: PARAM_GAIN,
            value: Some(0.0),
        })
        .unwrap();
    assert_eq!(
        plugin.set_params(&editor.project().devices[&inserts[0]].params.clone()),
        1
    );
    let after = render(&mut renderer, 2048);
    assert!(
        (after[1500 * 2] - both / 2.0).abs() < 1e-5,
        "{}",
        after[1500 * 2]
    );
    assert_eq!(cache.instances_created(), before + 3);

    // A full recompile finds the same instances.
    compile_project(&editor.project().clone(), &ctx, RATE, MAX_BLOCK).unwrap();
    assert_eq!(cache.instances_created(), before + 3);
    assert!(std::sync::Arc::ptr_eq(
        &plugin,
        &cache.get(inserts[0].0).unwrap()
    ));
}
