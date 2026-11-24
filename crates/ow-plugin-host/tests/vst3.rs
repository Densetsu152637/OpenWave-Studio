//! VST3 hosting through the fixture module (M-G16, VST3 part): scanning out of process,
//! processing, parameter edits that keep the instance, state round trips, latency, note events
//! and the tone instrument's single-component layout. (Editor-side parameter changes are in
//! `editor.rs`: the fixtures' test hooks act on whichever instance was made last, so they need a
//! test binary that runs one check at a time.)

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use ow_dsp::Event;
use ow_plugin_fixtures::{
    GAIN_LATENCY, PARAM_GAIN, PARAM_LEVEL, PARAM_POLARITY, VST3_GAIN_ID, VST3_TONE_ID,
};
use ow_plugin_host::{Format, ScanStatus, decode_state, encode_state};

use common::*;

#[test]
fn the_vst3_module_scans_with_its_buses_parameters_and_latency() {
    let report = scanner().scan(&[ow_plugin_fixtures::vst3_path()], false);
    assert!(report.failures().is_empty(), "{:#?}", report.files);
    assert_eq!(report.files[0].status, ScanStatus::Ok);
    let plugins = report.plugins();
    // The controller class is not a plugin of its own.
    assert_eq!(plugins.len(), 2, "{plugins:#?}");

    let gain = plugins.iter().find(|p| p.id == VST3_GAIN_ID).unwrap();
    assert_eq!(gain.format, Format::Vst3);
    assert_eq!(gain.kind, format!("vst3:{VST3_GAIN_ID}"));
    assert_eq!(gain.name, "OpenWave Fixture Gain VST3");
    assert_eq!(gain.vendor, "OpenWave Studio tests");
    assert_eq!(gain.version, "1.0.0");
    assert_eq!(gain.features, ["Fx"]);
    assert!(!gain.instrument && !gain.note_input);
    assert_eq!((gain.audio_inputs, gain.audio_outputs), (2, 2));
    assert_eq!(gain.latency, GAIN_LATENCY);
    assert!(gain.editor);
    let ids: Vec<u32> = gain.params.iter().map(|p| p.id).collect();
    assert_eq!(ids, [PARAM_GAIN, PARAM_POLARITY]);
    let gain_param = gain.param(PARAM_GAIN).unwrap();
    // Plain values come from the controller: normalised 0..1 maps to 0..2.
    assert_eq!(
        (gain_param.min, gain_param.max, gain_param.default),
        (0.0, 2.0, 1.0)
    );
    assert!(!gain_param.stepped && gain_param.choices.is_empty());
    let polarity = gain.param(PARAM_POLARITY).unwrap();
    assert!(polarity.stepped);
    assert_eq!(polarity.choices, ["Normal", "Inverted"]);

    let tone = plugins.iter().find(|p| p.id == VST3_TONE_ID).unwrap();
    assert!(tone.instrument && tone.note_input);
    assert_eq!(tone.features, ["Instrument", "Synth"]);
    assert_eq!((tone.audio_inputs, tone.audio_outputs), (0, 2));
    assert_eq!(tone.params.len(), 1);
    assert_eq!(tone.latency, 0);
}

#[test]
fn a_file_that_is_not_a_vst3_module_is_a_failed_record_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("fake.vst3");
    std::fs::write(&fake, b"not a library").unwrap();
    let record = scanner().scan_file(&fake);
    assert!(
        matches!(record.status, ScanStatus::Failed { .. }),
        "{record:?}"
    );
    // A bundle folder without a module for this platform is a failure too.
    let bundle = dir.path().join("empty.vst3");
    std::fs::create_dir_all(bundle.join("Contents")).unwrap();
    assert!(matches!(
        scanner().scan_file(&bundle).status,
        ScanStatus::Failed { .. }
    ));
}

#[test]
fn a_parameter_edit_keeps_the_same_instance_and_changes_the_sound() {
    let cache = fixture_cache();
    let kind = vst3_gain_kind();
    let before = cache.instances_created();
    let plugin = cache
        .acquire(7, &kind, &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    assert_eq!(cache.instances_created(), before + 1);
    assert_eq!(plugin.info().format, Format::Vst3);
    let mut processor = plugin.processor();

    // Delayed by the plugin's latency, at gain 1.
    let (left, right) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    let at = GAIN_LATENCY as usize;
    assert_eq!(left[..at], vec![0.0; at][..]);
    assert_eq!(left[at], 1.0);
    assert_eq!(right[at], 1.0);

    // An edit reaches the running instance (plain 0.5 is normalised 0.25 for the processor).
    let edit = BTreeMap::from([(PARAM_GAIN, 0.5)]);
    assert_eq!(plugin.set_params(&edit), 1);
    assert_eq!(
        plugin.set_params(&edit),
        0,
        "an unchanged value queues nothing"
    );
    let (left, _) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    assert_eq!(left[at], 0.5);
    // The controller was told too, and maps back to the plain value.
    assert_eq!(plugin.param_value(PARAM_GAIN).unwrap(), Some(0.5));

    // A recompile asks for the same device again: the same instance comes back, with the
    // model's values applied.
    let edit = BTreeMap::from([(PARAM_GAIN, 0.25), (PARAM_POLARITY, 1.0)]);
    let again = cache.acquire(7, &kind, &edit, None, RATE, BLOCK).unwrap();
    assert!(Arc::ptr_eq(&plugin, &again));
    assert_eq!(cache.instances_created(), before + 1);
    let (left, _) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    assert_eq!(left[at], -0.25);
    assert_eq!(plugin.param_value(PARAM_POLARITY).unwrap(), Some(1.0));

    // A parameter that is no longer set goes back to its default.
    plugin.set_params(&BTreeMap::new());
    let (left, _) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    assert_eq!(left[at], 1.0);

    // Removing the device releases the instance; its processor goes quiet instead of crashing,
    // and an effect passes its input through.
    cache.retain(&Default::default());
    assert!(cache.is_empty());
    let (left, _) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    assert_eq!(left, impulse());
}

#[test]
fn a_new_device_of_another_kind_replaces_the_instance_and_unknown_classes_fail() {
    let cache = fixture_cache();
    let params = BTreeMap::new();
    let first = cache
        .acquire(1, &vst3_gain_kind(), &params, None, RATE, BLOCK)
        .unwrap();
    let second = cache
        .acquire(1, &vst3_tone_kind(), &params, None, RATE, BLOCK)
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(cache.len(), 1);
    assert_eq!(second.info().id, VST3_TONE_ID);
    assert!(first.save_state().is_err(), "the released instance is gone");
    let missing = format!("vst3:{}", "0".repeat(32));
    assert!(
        cache
            .acquire(2, &missing, &params, None, RATE, BLOCK)
            .is_err()
    );
}

#[test]
fn state_round_trips_through_a_second_instance() {
    let cache = fixture_cache();
    let kind = vst3_gain_kind();
    let plugin = cache
        .acquire(1, &kind, &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    plugin.set_params(&BTreeMap::from([(PARAM_GAIN, 0.75), (PARAM_POLARITY, 1.0)]));
    let mut processor = plugin.processor();
    run(processor.as_mut(), &impulse(), &impulse(), &[]);

    let state = plugin.save_state().unwrap();
    assert!(state.starts_with(b"OWV3"), "{state:?}");
    let text = encode_state(&state);
    assert_eq!(decode_state(&text).unwrap(), state);

    // A fresh instance (another device, as after reopening a project) restores the component
    // and the controller state before it is activated.
    let other = cache
        .acquire(2, &kind, &BTreeMap::new(), Some(state.clone()), RATE, BLOCK)
        .unwrap();
    assert_eq!(other.param_value(PARAM_GAIN).unwrap(), Some(0.75));
    assert_eq!(other.param_value(PARAM_POLARITY).unwrap(), Some(1.0));
    assert_eq!(other.save_state().unwrap(), state);
    let mut restored = other.processor();
    let (left, _) = run(restored.as_mut(), &impulse(), &impulse(), &[]);
    assert_eq!(
        left[GAIN_LATENCY as usize], -0.75,
        "the processor restored its own state"
    );

    // Loading into a live instance works, and bad state is refused without harm.
    let third = cache
        .acquire(3, &kind, &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    assert_eq!(third.param_value(PARAM_GAIN).unwrap(), Some(1.0));
    third.load_state(&state).unwrap();
    assert_eq!(third.param_value(PARAM_GAIN).unwrap(), Some(0.75));
    assert!(third.load_state(b"garbage").is_err());
    assert_eq!(third.param_value(PARAM_GAIN).unwrap(), Some(0.75));

    let states = cache.collect_states();
    assert_eq!(states.len(), 3);
    assert!(states.iter().all(|(_, format, _)| *format == Format::Vst3));
}

#[test]
fn the_reported_latency_is_available_for_delay_compensation() {
    let cache = fixture_cache();
    let plugin = cache
        .acquire(1, &vst3_gain_kind(), &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    assert_eq!(plugin.latency_frames(), GAIN_LATENCY);
    assert_eq!(plugin.processor().latency_frames(), GAIN_LATENCY);
    let tone = cache
        .acquire(2, &vst3_tone_kind(), &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    assert_eq!(tone.latency_frames(), 0);
}

#[test]
fn an_instrument_plays_note_events_at_their_sample_offsets() {
    let cache = fixture_cache();
    let plugin = cache
        .acquire(
            1,
            &vst3_tone_kind(),
            &BTreeMap::from([(PARAM_LEVEL, 0.5)]),
            None,
            RATE,
            BLOCK,
        )
        .unwrap();
    let mut processor = plugin.processor();
    let silence = vec![0.0; BLOCK];
    let events = [Event::note_on(100, 60, 127), Event::note_off(200, 60)];
    let (left, right) = run(processor.as_mut(), &silence, &silence, &events);
    assert!(left[..100].iter().all(|s| *s == 0.0));
    assert!(
        left[100..200].iter().all(|s| *s == 0.5),
        "{:?}",
        &left[95..105]
    );
    assert!(left[200..].iter().all(|s| *s == 0.0));
    assert_eq!(left, right);

    // A held note carries over into the next block, and the level parameter follows edits.
    let (left, _) = run(
        processor.as_mut(),
        &silence,
        &silence,
        &[Event::note_on(0, 61, 127)],
    );
    assert!(left.iter().all(|s| *s == 0.5));
    plugin.set_params(&BTreeMap::from([(PARAM_LEVEL, 0.25)]));
    let (left, _) = run(processor.as_mut(), &silence, &silence, &[]);
    assert!(left.iter().all(|s| *s == 0.25), "{:?}", &left[..4]);
    let loud = vec![1.0; BLOCK];
    let (left, _) = run(processor.as_mut(), &loud, &loud, &[Event::note_off(0, 61)]);
    assert!(left.iter().all(|s| *s == 0.0));
}
