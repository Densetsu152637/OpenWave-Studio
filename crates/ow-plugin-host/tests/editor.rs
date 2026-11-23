//! Plugin editor windows (Windows and macOS): creating, attaching and closing an editor for a
//! CLAP and a VST3 plugin, honouring the editor's resize requests, and an editor-side parameter
//! change arriving at the host with its gesture.
//!
//! This test has its own `main` (`harness = false`): AppKit only lets the process's main thread
//! create windows, and the test harness runs tests on other threads. The main thread here plays
//! the app's event loop (it runs the plugin main-thread jobs and the window messages) while the
//! checks run on a second thread and call the plugins like the control layer does.
//!
//! On Linux (no editor windows) the test only checks that the editor is refused.

mod common;

use std::collections::BTreeMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;

use ow_plugin_fixtures::{GAIN_LATENCY, PARAM_GAIN, hooks};
use ow_plugin_host::editor::{self, EditorWindow};
use ow_plugin_host::executor::{ExecutorError, Job};
use ow_plugin_host::{EditPhase, EditorEdit, MainThreadExecutor, PluginCache, executor};

use common::*;

/// The main thread as the app's event loop: jobs sent here run on it.
struct MainLoop {
    sender: Mutex<mpsc::Sender<Job>>,
    thread: ThreadId,
}

impl MainThreadExecutor for MainLoop {
    fn dispatch(&self, job: Job) -> Result<(), ExecutorError> {
        self.sender
            .lock()
            .unwrap()
            .send(job)
            .map_err(|_| ExecutorError::Gone)
    }

    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.thread
    }
}

fn main() {
    let (sender, receiver) = mpsc::channel::<Job>();
    let executor: Arc<MainLoop> = Arc::new(MainLoop {
        sender: Mutex::new(sender),
        thread: thread::current().id(),
    });
    let worker = {
        let executor = Arc::clone(&executor);
        thread::spawn(move || {
            let executor: Arc<dyn MainThreadExecutor> = executor;
            run_checks(&executor);
        })
    };
    loop {
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(job) => job(),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {}
        }
        EditorWindow::pump_messages();
        if worker.is_finished() {
            break;
        }
    }
    match worker.join() {
        Ok(()) => println!("editor checks passed"),
        Err(_) => {
            eprintln!("editor checks failed");
            std::process::exit(1);
        }
    }
}

fn run_checks(executor: &Arc<dyn MainThreadExecutor>) {
    let cache = cache_on(Arc::clone(executor));
    // A change made in a plugin's editor reaches the host even where there is no window to show.
    check_gesture("CLAP", &cache, &gain_kind());
    check_gesture("VST3", &cache, &vst3_gain_kind());
    if !editor::AVAILABLE {
        let plugin = cache
            .acquire(1, &gain_kind(), &BTreeMap::new(), None, RATE, BLOCK)
            .unwrap();
        assert!(plugin.open_editor().is_err(), "Linux has no editor windows");
        assert!(!plugin.editor_open());
        println!("no editor windows on this platform: refused");
        return;
    }
    check_editor("CLAP", &cache, &gain_kind());
    check_editor("VST3", &cache, &vst3_gain_kind());
    println!("project close and device removal close the editors");
}

/// A gesture in the plugin's editor, without a window: the host receives its phases and values
/// and passes the change to the plugin's processor. (The editor's controller exists without a
/// window, which is also how a plugin behaves while the user has its editor closed and a
/// program change moves a knob.)
fn check_gesture(format: &str, cache: &PluginCache, kind: &str) {
    println!("{format}: editor gesture");
    let plugin = cache
        .acquire(9, kind, &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    let edits = Arc::new(Mutex::new(Vec::<EditorEdit>::new()));
    {
        let edits = Arc::clone(&edits);
        plugin.set_edit_sink(Arc::new(move |edit| edits.lock().unwrap().push(edit)));
    }
    let mut processor = plugin.processor();
    simulate(cache, module_of(kind), hooks::PHASE_BEGIN, 0.0);
    simulate(cache, module_of(kind), hooks::PHASE_CHANGE, 0.5);
    simulate(cache, module_of(kind), hooks::PHASE_CHANGE, 0.75);
    simulate(cache, module_of(kind), hooks::PHASE_END, 0.0);
    let (left, _) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    // CLAP reports from the audio thread: the host serves it from its service pass.
    plugin.service();
    assert_eq!(
        left[GAIN_LATENCY as usize], 0.75,
        "{format}: the sound follows"
    );
    let seen = edits.lock().unwrap().clone();
    let shapes: Vec<(EditPhase, f64)> = seen.iter().map(|e| (e.phase, e.value)).collect();
    assert_eq!(
        shapes,
        [
            (EditPhase::Begin, 1.0),
            (EditPhase::Change, 0.5),
            (EditPhase::Change, 0.75),
            (EditPhase::End, 0.75),
        ],
        "{format}: {seen:?}"
    );
    assert!(seen.iter().all(|edit| edit.param == PARAM_GAIN));
    // The model hands the value back as SetDeviceParam: the plugin already has it.
    assert_eq!(plugin.set_params(&BTreeMap::from([(PARAM_GAIN, 0.75)])), 0);
    assert_eq!(plugin.param_value(PARAM_GAIN).unwrap(), Some(0.75));
    cache.clear();
}

/// Runs `work` on the plugin main thread.
fn on_main<T: Send + 'static>(cache: &PluginCache, work: impl FnOnce() -> T + Send + 'static) -> T {
    executor::call(cache.executor().as_ref(), work).unwrap()
}

fn check_editor(format: &str, cache: &PluginCache, kind: &str) {
    println!("{format}: open, resize, edit, close");
    let plugin = cache
        .acquire(1, kind, &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    let edits = Arc::new(Mutex::new(Vec::<EditorEdit>::new()));
    {
        let edits = Arc::clone(&edits);
        plugin.set_edit_sink(Arc::new(move |edit| edits.lock().unwrap().push(edit)));
    }
    assert!(plugin.info().editor, "{format}: the scan found an editor");
    assert!(!plugin.editor_open());
    assert_eq!(hooks_state(cache, module_of(kind)).attached, 0);

    // Open: the window is created and the plugin's view attached, at the size the plugin asked.
    plugin.open_editor().unwrap();
    assert!(plugin.editor_open());
    let state = hooks_state(cache, module_of(kind));
    assert_eq!(state.attached, 1, "{format}: the view is attached");
    assert_eq!((state.width, state.height), (320, 200));
    assert_eq!(plugin.editor_size(), Some((320, 200)));
    // Opening again only brings the window forward.
    plugin.open_editor().unwrap();
    assert_eq!(hooks_state(cache, module_of(kind)).attached, 1);

    // The editor asks for a larger window: the host honours it and the view follows.
    assert!(request_resize(cache, module_of(kind), 400, 300));
    assert_eq!(plugin.editor_size(), Some((400, 300)));
    let state = hooks_state(cache, module_of(kind));
    assert_eq!((state.width, state.height), (400, 300));

    // A change made in the editor: a gesture with its value, which the host passes to the
    // plugin's processor as well.
    let mut processor = plugin.processor();
    simulate(cache, module_of(kind), hooks::PHASE_BEGIN, 0.0);
    simulate(cache, module_of(kind), hooks::PHASE_CHANGE, 0.5);
    simulate(cache, module_of(kind), hooks::PHASE_END, 0.0);
    let (left, _) = run(processor.as_mut(), &impulse(), &impulse(), &[]);
    // CLAP reports from the audio thread: the host serves it from its service pass.
    plugin.service();
    assert_eq!(
        left[GAIN_LATENCY as usize], 0.5,
        "{format}: the sound follows"
    );
    let seen = edits.lock().unwrap().clone();
    let shapes: Vec<(EditPhase, f64)> = seen.iter().map(|e| (e.phase, e.value)).collect();
    assert_eq!(
        shapes,
        [
            (EditPhase::Begin, 1.0),
            (EditPhase::Change, 0.5),
            (EditPhase::End, 0.5)
        ],
        "{format}: {seen:?}"
    );
    assert!(seen.iter().all(|edit| edit.param == PARAM_GAIN));
    // The model hands the value back as SetDeviceParam: nothing to do for the plugin.
    assert_eq!(plugin.set_params(&BTreeMap::from([(PARAM_GAIN, 0.5)])), 0);
    // The service pass also notices window events; none happened.
    plugin.service();
    assert!(plugin.editor_open());

    // Close: the view is removed and the window destroyed.
    plugin.close_editor();
    assert!(!plugin.editor_open());
    assert_eq!(plugin.editor_size(), None);
    assert_eq!(hooks_state(cache, module_of(kind)).attached, 0);
    plugin.close_editor();

    // Removing the device closes an open editor.
    plugin.open_editor().unwrap();
    assert_eq!(hooks_state(cache, module_of(kind)).attached, 1);
    cache.retain(&Default::default());
    assert_eq!(
        hooks_state(cache, module_of(kind)).attached,
        0,
        "{format}: removing the device closed its editor"
    );
    assert!(!plugin.editor_open());
    assert!(
        plugin.open_editor().is_err(),
        "a released plugin has no editor"
    );

    // So does closing the project.
    let plugin = cache
        .acquire(2, kind, &BTreeMap::new(), None, RATE, BLOCK)
        .unwrap();
    plugin.open_editor().unwrap();
    assert_eq!(hooks_state(cache, module_of(kind)).attached, 1);
    cache.clear();
    assert_eq!(hooks_state(cache, module_of(kind)).attached, 0);
}

fn module_of(kind: &str) -> std::path::PathBuf {
    if kind.starts_with("vst3:") {
        ow_plugin_fixtures::vst3_path()
    } else {
        ow_plugin_fixtures::gain_path()
    }
}

fn hooks_state(
    cache: &PluginCache,
    module: std::path::PathBuf,
) -> ow_plugin_fixtures::hooks::EditorState {
    on_main(cache, move || Hooks::of(&module).editor_state())
}

fn request_resize(
    cache: &PluginCache,
    module: std::path::PathBuf,
    width: u32,
    height: u32,
) -> bool {
    on_main(cache, move || {
        Hooks::of(&module).request_resize(width, height)
    })
}

fn simulate(cache: &PluginCache, module: std::path::PathBuf, phase: u32, value: f64) {
    on_main(cache, move || {
        Hooks::of(&module).simulate_edit(PARAM_GAIN, value, phase)
    });
}
