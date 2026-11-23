//! Hosted plugins through the control layer (slice M8): a parameter edit reaches the running
//! plugin without a recompile and without a new instance, and a plugin's state is saved with the
//! project and restored when it is opened, without adding an undo step or making the project
//! dirty.

use std::sync::Arc;

use ow_engine::{MAX_BLOCK, compile_project};
use ow_ipc::{EditCommand, UiCommand};
use ow_model::{Device, DeviceId, Owned, Strip, Track, TrackId, TrackKind};
use ow_plugin_fixtures::{GAIN_ID, PARAM_GAIN, PARAM_POLARITY};
use ow_plugin_host::{
    HostedPlugin, PluginCache, ThreadExecutor, decode_state, instantiation_count, scan,
};

use super::control::PlanTarget;
use super::mvp::{CompilingTarget, command, edit};
use super::{IpcState, lock};

const KIND: &str = "clap:com.openwave.fixture.gain";

/// A cache whose directory holds the fixture plugins, scanned in this process (the scanner
/// child has its own tests in `ow-plugin-host`).
fn fixture_cache() -> PluginCache {
    let cache = PluginCache::new(Arc::new(ThreadExecutor::new()));
    let plugins = scan::scan_in_process(&ow_plugin_fixtures::gain_path()).expect("scan fixtures");
    cache.directory().set(plugins);
    cache
}

/// An audio track whose only insert is the fixture gain plugin; returns the plugin's device id.
fn add_gain_track(state: &IpcState, target: &dyn PlanTarget) -> DeviceId {
    let ids = command(state, target, UiCommand::AllocIds { count: 2 });
    let (track, device) = (TrackId(ids[0]), DeviceId(ids[1]));
    edit(
        state,
        target,
        EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned {
                item: Track {
                    id: track,
                    name: "Audio".into(),
                    color: 0,
                    kind: TrackKind::Audio,
                    strip: Strip {
                        inserts: vec![device],
                        ..Strip::default()
                    },
                    instrument: None,
                },
                clips: vec![],
                devices: vec![Device {
                    id: device,
                    kind: KIND.into(),
                    name: "Gain".into(),
                    enabled: true,
                    params: Default::default(),
                    state: vec![],
                    sidechain: None,
                }],
                lanes: vec![],
            }),
        },
    );
    device
}

/// Lets the queued parameter edits reach the plugin by running one block through it, then
/// reads the plugin's own value.
fn value_after_a_block(plugin: &Arc<dyn HostedPlugin>, param: u32) -> f64 {
    let mut processor = plugin.processor();
    let mut left = vec![0.0; 64];
    let mut right = vec![0.0; 64];
    processor.process(&mut [&mut left, &mut right], &[]);
    plugin.param_value(param).unwrap().unwrap()
}

fn set_param(state: &IpcState, target: &dyn PlanTarget, device: DeviceId, param: u32, value: f64) {
    edit(
        state,
        target,
        EditCommand::SetDeviceParam {
            device,
            param,
            value: Some(value),
        },
    );
}

#[test]
fn plugin_state_a_parameter_edit_needs_no_recompile_and_keeps_the_instance() {
    let cache = fixture_cache();
    let state = IpcState::with_plugins(cache.clone());
    let target = CompilingTarget::default();
    let device = add_gain_track(&state, &target);
    let compiles = target.applied.borrow().len();
    let instances = instantiation_count();
    let plugin = cache.get(device.0).expect("the plugin was instantiated");
    assert_eq!(plugin.info().id, GAIN_ID);

    set_param(&state, &target, device, PARAM_GAIN, 0.5);
    set_param(&state, &target, device, PARAM_POLARITY, 1.0);
    assert_eq!(
        target.applied.borrow().len(),
        compiles,
        "a plugin parameter edit must not recompile the project"
    );
    assert_eq!(
        instantiation_count(),
        instances,
        "nor re-instantiate the plugin"
    );
    assert_eq!(value_after_a_block(&plugin, PARAM_GAIN), 0.5);
    assert_eq!(value_after_a_block(&plugin, PARAM_POLARITY), 1.0);
    // The model holds the values (undo and the UI mirror see them).
    let params = lock(&state.control).session().project().devices[&device]
        .params
        .clone();
    assert_eq!(params.get(&PARAM_GAIN), Some(&0.5));

    // Undo takes the same path: the plugin goes back to its default.
    lock(&state.control)
        .command(UiCommand::Undo, &target)
        .unwrap();
    lock(&state.control)
        .command(UiCommand::Undo, &target)
        .unwrap();
    assert_eq!(target.applied.borrow().len(), compiles);
    assert_eq!(value_after_a_block(&plugin, PARAM_GAIN), 1.0);
    assert_eq!(value_after_a_block(&plugin, PARAM_POLARITY), 0.0);
    assert!(Arc::ptr_eq(&plugin, &cache.get(device.0).unwrap()));

    // Other edits (renaming the device is not one that changes the sound, bypassing it is)
    // still recompile.
    edit(
        &state,
        &target,
        EditCommand::SetDeviceEnabled {
            device,
            enabled: false,
        },
    );
    assert_eq!(target.applied.borrow().len(), compiles + 1);
}

#[test]
fn plugin_state_is_saved_with_the_project_and_restored_on_open() {
    let cache = fixture_cache();
    let state = IpcState::with_plugins(cache.clone());
    let target = CompilingTarget::default();
    let device = add_gain_track(&state, &target);
    set_param(&state, &target, device, PARAM_GAIN, 0.25);
    set_param(&state, &target, device, PARAM_POLARITY, 1.0);
    let plugin = cache.get(device.0).unwrap();
    assert_eq!(value_after_a_block(&plugin, PARAM_GAIN), 0.25);

    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("plugin-song");
    let before = lock(&state.control).snapshot();
    assert!(before.history.dirty);
    state.project_save(Some(&bundle), &target).unwrap();

    // The saved file holds the plugin's state next to the parameters ...
    let saved = ow_model::bundle::load(&dir.path().join("plugin-song.owave")).unwrap();
    let saved_device = &saved.devices[&device];
    assert_eq!(saved_device.params.get(&PARAM_GAIN), Some(&0.25));
    assert_eq!(saved_device.state.len(), 1);
    assert_eq!(saved_device.state[0].format, "clap");
    let bytes = decode_state(&saved_device.state[0].data).expect("base64 state");
    assert_eq!(bytes, plugin.save_state().unwrap());

    // ... and the live model was not touched: no undo entry, the project is simply saved.
    let after = lock(&state.control).snapshot();
    assert_eq!(after.project.devices[&device].state, Vec::new());
    assert_eq!(after.history.can_undo, before.history.can_undo);
    assert_eq!(after.history.undo_label, before.history.undo_label);
    assert!(!after.history.dirty);

    // A new app (a fresh cache, as after a restart) opens it: the plugin comes back with its
    // parameters and its own state.
    let reopened_cache = fixture_cache();
    let reopened = IpcState::with_plugins(reopened_cache.clone());
    let reopened_target = CompilingTarget::default();
    let snapshot = reopened
        .project_open(&dir.path().join("plugin-song.owave"), &reopened_target)
        .unwrap();
    assert_eq!(
        snapshot.project.devices[&device].params.get(&PARAM_GAIN),
        Some(&0.25)
    );
    let restored = reopened_cache.get(device.0).expect("instantiated on open");
    assert_eq!(restored.param_value(PARAM_GAIN).unwrap(), Some(0.25));
    assert_eq!(restored.param_value(PARAM_POLARITY).unwrap(), Some(1.0));
    assert_eq!(restored.save_state().unwrap(), bytes);

    // Opening another project releases the old project's instances.
    reopened.project_new(&reopened_target).unwrap();
    assert!(reopened_cache.is_empty());
}

#[test]
fn plugin_state_an_export_renders_its_own_instances_and_leaves_the_live_ones() {
    let cache = fixture_cache();
    let state = IpcState::with_plugins(cache.clone());
    let target = CompilingTarget::default();
    let device = add_gain_track(&state, &target);
    set_param(&state, &target, device, PARAM_GAIN, 0.5);
    let live = cache.get(device.0).unwrap();
    let before = instantiation_count();

    let dir = tempfile::tempdir().unwrap();
    state.export_wav(&dir.path().join("mix.wav")).unwrap();
    assert!(
        instantiation_count() > before,
        "the render made its own instance"
    );
    assert_eq!(cache.len(), 1);
    assert!(Arc::ptr_eq(&live, &cache.get(device.0).unwrap()));

    // The compiled plan for the offline render is the same as the engine's: latency included.
    let (project, ctx) = lock(&state.control).offline_snapshot();
    let compiled = compile_project(&project, &ctx, 48_000, MAX_BLOCK).unwrap();
    assert_eq!(
        compiled.plan.output_latency(),
        ow_plugin_fixtures::GAIN_LATENCY
    );
}
