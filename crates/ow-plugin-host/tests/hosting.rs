//! CLAP hosting through the fixture plugins (M-G16, CLAP part): scanning out of process and
//! surviving a crashing plugin, parameter edits that keep the same instance, state round trips,
//! latency, note events, and the main-thread rules.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ow_dsp::Event;
use ow_plugin_fixtures::{
    CRASH_ID, CRASH_MODE_ENV, GAIN_ID, GAIN_LATENCY, PARAM_GAIN, PARAM_LEVEL, PARAM_POLARITY,
    TONE_ID,
};
use ow_plugin_host::{
    ScanCommand, ScanStatus, Scanner, decode_state, encode_state, instantiation_count,
};

use common::*;

#[test]
fn a_scan_through_ow_scan_survives_the_crashing_plugin() {
    let bundles = ow_plugin_fixtures::bundle_dir();
    let cache_file = tempfile::tempdir().unwrap();
    let scanner = scanner().with_cache(cache_file.path().join("plugin-cache.json"));
    let report = scanner.scan(std::slice::from_ref(&bundles), false);

    // Both bundles were looked at; the crashing one is a record, not a dead scanner.
    let by_name = |name: &str| {
        report
            .files
            .iter()
            .find(|file| file.path.ends_with(name))
            .unwrap_or_else(|| panic!("no record for {name}: {:#?}", report.files))
    };
    assert_eq!(
        by_name(ow_plugin_fixtures::GAIN_FILE).status,
        ScanStatus::Ok
    );
    assert!(
        matches!(
            by_name(ow_plugin_fixtures::CRASH_FILE).status,
            ScanStatus::Crashed { .. }
        ),
        "{:?}",
        by_name(ow_plugin_fixtures::CRASH_FILE)
    );
    assert_eq!(report.scanned, 2);
    assert_eq!(report.failures().len(), 1);

    // The good plugins are all there with their parameters, ports and latency.
    let plugins = report.plugins();
    assert_eq!(plugins.len(), 2, "{plugins:#?}");
    let gain = plugins.iter().find(|p| p.id == GAIN_ID).unwrap();
    assert_eq!(gain.kind, "clap:com.openwave.fixture.gain");
    assert_eq!(gain.name, "OpenWave Fixture Gain");
    assert_eq!(gain.vendor, "OpenWave Studio tests");
    assert!(!gain.instrument);
    assert_eq!((gain.audio_inputs, gain.audio_outputs), (2, 2));
    assert_eq!(gain.latency, GAIN_LATENCY);
    let ids: Vec<u32> = gain.params.iter().map(|p| p.id).collect();
    assert_eq!(ids, [PARAM_GAIN, PARAM_POLARITY]);
    let polarity = gain.param(PARAM_POLARITY).unwrap();
    assert!(polarity.stepped);
    assert_eq!(polarity.choices, ["Normal", "Inverted"]);
    let gain_param = gain.param(PARAM_GAIN).unwrap();
    assert_eq!(
        (gain_param.min, gain_param.max, gain_param.default),
        (0.0, 2.0, 1.0)
    );
    let tone = plugins.iter().find(|p| p.id == TONE_ID).unwrap();
    assert!(tone.instrument && tone.note_input);
    assert_eq!((tone.audio_inputs, tone.audio_outputs), (0, 2));

    // The cache holds the crash too, keyed by path, size and mtime: a second scan starts no
    // child at all, and the crashing file is not tried again.
    let again = scanner.scan(&[bundles], false);
    assert_eq!((again.scanned, again.cached), (0, 2));
    assert_eq!(again.files, report.files);
}

#[test]
fn a_plugin_that_hangs_times_out_and_is_blacklisted() {
    let scanner = Scanner::new(ScanCommand::new(ow_scan()))
        .with_timeout(Duration::from_millis(1500))
        .with_env(CRASH_MODE_ENV, "hang");
    let started = std::time::Instant::now();
    let record = scanner.scan_file(&ow_plugin_fixtures::crash_path());
    assert_eq!(record.status, ScanStatus::TimedOut, "{record:?}");
    assert!(started.elapsed() < Duration::from_secs(8));
    assert!(record.plugins.is_empty());
    assert_eq!(CRASH_ID, "com.openwave.fixture.crash");
}

#[test]
fn a_file_that_is_not_a_plugin_is_a_failed_record_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("fake.clap");
    std::fs::write(&fake, b"not a library").unwrap();
    let record = scanner().scan_file(&fake);
    assert!(
        matches!(record.status, ScanStatus::Failed { .. }),
        "{record:?}"
    );
}

#[test]
fn a_parameter_edit_keeps_the_same_instance_and_changes_the_sound() {
    let cache = fixture_cache();
    let kind = gain_kind();
    let before = instantiation_count();
    let plugin = cache
        .acquire(7, &kind, &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    assert_eq!(instantiation_count(), before + 1);
    let mut processor = plugin.processor();

    // The plugin delays by its latency: an impulse at frame 0 comes out at frame 64, at gain 1.
    let (left, right) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    let at = GAIN_LATENCY as usize;
    assert_eq!(left[..at], vec![0.0; at][..]);
    assert_eq!(left[at], 1.0);
    assert_eq!(right[at], 1.0);

    // An edit reaches the running instance as a parameter event; nothing is re-instantiated.
    let edit = BTreeMap::from([(PARAM_GAIN, 0.5)]);
    assert_eq!(plugin.set_params(&edit), 1);
    assert_eq!(
        plugin.set_params(&edit),
        0,
        "an unchanged value queues nothing"
    );
    let (left, _) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    assert_eq!(left[at], 0.5);
    assert_eq!(plugin.param_value(PARAM_GAIN).unwrap(), Some(0.5));

    // A recompile asks for the same device again: the same instance comes back, with the
    // model's values applied.
    let edit = BTreeMap::from([(PARAM_GAIN, 0.25), (PARAM_POLARITY, 1.0)]);
    let again = cache.acquire(7, &kind, &edit, None, RATE, BLOCK).unwrap();
    assert!(Arc::ptr_eq(&plugin, &again));
    assert_eq!(instantiation_count(), before + 1);
    let (left, _) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    assert_eq!(left[at], -0.25);
    assert_eq!(plugin.param_value(PARAM_POLARITY).unwrap(), Some(1.0));

    // A parameter that is no longer set goes back to its default.
    plugin.set_params(&BTreeMap::new());
    let (left, _) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    assert_eq!(left[at], 1.0);

    // Removing the device from the project releases the instance; its processor goes quiet
    // instead of crashing, and an effect passes its input through.
    cache.retain(&Default::default());
    assert!(cache.is_empty());
    let (left, _) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    assert_eq!(left, impulse());
}

#[test]
fn a_new_device_of_another_kind_replaces_the_instance() {
    let cache = fixture_cache();
    let params = BTreeMap::new();
    let first = cache
        .acquire(1, &gain_kind(), &params, None, RATE, BLOCK)
        .unwrap();
    let second = cache
        .acquire(1, &tone_kind(), &params, None, RATE, BLOCK)
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(cache.len(), 1);
    assert_eq!(second.info().id, TONE_ID);
    // The released instance refuses further main-thread calls.
    assert!(first.save_state().is_err());
    assert!(
        cache
            .acquire(2, "clap:com.not.installed", &params, None, RATE, BLOCK)
            .is_err()
    );
}

#[test]
fn state_round_trips_through_a_second_instance() {
    let cache = fixture_cache();
    let kind = gain_kind();
    let plugin = cache
        .acquire(1, &kind, &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    plugin.set_params(&BTreeMap::from([(PARAM_GAIN, 0.75), (PARAM_POLARITY, 1.0)]));
    // The queued edits reach the plugin when a block is processed.
    let mut processor = plugin.processor();
    run(processor.as_mut(), &impulse(), &impulse(), &[]);

    let state = plugin.save_state().unwrap();
    assert!(state.starts_with(b"OWFX"), "{state:?}");
    // The project stores it as base64 text.
    let text = encode_state(&state);
    assert_eq!(decode_state(&text).unwrap(), state);

    // A fresh instance (another device, as after reopening a project) restores it.
    let other = cache
        .acquire(2, &kind, &BTreeMap::new(), Some(state.clone()), RATE, BLOCK)
        .unwrap();
    assert_eq!(other.param_value(PARAM_GAIN).unwrap(), Some(0.75));
    assert_eq!(other.param_value(PARAM_POLARITY).unwrap(), Some(1.0));
    assert_eq!(other.save_state().unwrap(), state);

    // Loading into a live instance works too, and bad state is refused without harm.
    let third = cache
        .acquire(3, &kind, &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    assert_eq!(third.param_value(PARAM_GAIN).unwrap(), Some(1.0));
    third.load_state(&state).unwrap();
    assert_eq!(third.param_value(PARAM_GAIN).unwrap(), Some(0.75));
    assert!(third.load_state(b"garbage").is_err());
    assert_eq!(third.param_value(PARAM_GAIN).unwrap(), Some(0.75));

    // Every live instance's state, for the app's save.
    let states = cache.collect_states();
    assert_eq!(states.len(), 3);
    assert_eq!(states[0].0, 1);
}

#[test]
fn the_reported_latency_is_available_for_delay_compensation() {
    let cache = fixture_cache();
    let plugin = cache
        .acquire(1, &gain_kind(), &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    assert_eq!(plugin.latency_frames(), GAIN_LATENCY);
    assert_eq!(plugin.processor().latency_frames(), GAIN_LATENCY);
    let tone = cache
        .acquire(2, &tone_kind(), &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    assert_eq!(tone.latency_frames(), 0);
}

#[test]
fn an_instrument_plays_note_events_at_their_sample_offsets() {
    let cache = fixture_cache();
    let plugin = cache
        .acquire(
            1,
            &tone_kind(),
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

    // A held note carries over into the next block.
    let (left, _) = run(
        processor.as_mut(),
        &silence,
        &silence,
        &[Event::note_on(0, 61, 127)],
    );
    assert!(left.iter().all(|s| *s == 0.5));
    // And an instrument's input is ignored.
    let loud = vec![1.0; BLOCK];
    let (left, _) = run(processor.as_mut(), &loud, &loud, &[Event::note_off(0, 61)]);
    assert!(left.iter().all(|s| *s == 0.0));
}

#[test]
fn main_thread_calls_run_on_the_executors_thread_and_a_fork_has_its_own_instances() {
    let cache = fixture_cache();
    let plugin = cache
        .acquire(1, &gain_kind(), &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    assert!(!cache.executor().is_main_thread());
    let fork = cache.fork();
    assert!(fork.is_empty());
    let other = fork
        .acquire(1, &gain_kind(), &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    assert!(!Arc::ptr_eq(&plugin, &other));
    drop(fork);
    // The live instance is untouched by the fork's release.
    assert_eq!(plugin.param_value(PARAM_GAIN).unwrap(), Some(1.0));
    assert_eq!(cache.len(), 1);
}
