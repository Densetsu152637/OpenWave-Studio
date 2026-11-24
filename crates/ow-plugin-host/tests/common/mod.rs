//! Shared helpers: scanning the fixture bundles and driving a processor by hand.

#![allow(dead_code, unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;

use ow_dsp::Event;
use ow_engine::{Message, OfflineRenderer, TransportCommand};
use ow_model::{
    Clip, ClipContent, Device, DeviceId, EditCommand, Editor, Note, Owned, Strip, Track, TrackId,
    TrackKind,
};
use ow_plugin_host::{
    MainThreadExecutor, PluginCache, PluginProcessor, ScanCommand, Scanner, ThreadExecutor,
};

pub const RATE: u32 = 48_000;
pub const BLOCK: usize = 256;

/// The built `ow-scan` child binary.
pub fn ow_scan() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ow-scan"))
}

pub fn scanner() -> Scanner {
    Scanner::new(ScanCommand::new(ow_scan()))
}

/// A cache on its own plugin main thread whose directory holds the fixture plugins, CLAP and
/// VST3 (found by scanning the bundles with the `ow-scan` child).
pub fn fixture_cache() -> PluginCache {
    cache_on(Arc::new(ThreadExecutor::new()))
}

/// Like [`fixture_cache`], with plugin main-thread work on `executor`.
pub fn cache_on(executor: Arc<dyn MainThreadExecutor>) -> PluginCache {
    let cache = PluginCache::new(executor);
    let report = scanner().scan(
        &[
            ow_plugin_fixtures::gain_path(),
            ow_plugin_fixtures::vst3_path(),
        ],
        false,
    );
    assert!(
        report.failures().is_empty(),
        "the fixtures must scan: {:?}",
        report.files
    );
    cache.directory().set(report.plugins());
    cache
}

pub fn gain_kind() -> String {
    format!("clap:{}", ow_plugin_fixtures::GAIN_ID)
}

pub fn tone_kind() -> String {
    format!("clap:{}", ow_plugin_fixtures::TONE_ID)
}

pub fn vst3_gain_kind() -> String {
    format!("vst3:{}", ow_plugin_fixtures::VST3_GAIN_ID)
}

pub fn vst3_tone_kind() -> String {
    format!("vst3:{}", ow_plugin_fixtures::VST3_TONE_ID)
}

/// The test hooks a fixture library exports (it acts as the user of the plugin's editor). The
/// library is the one the host already loaded: loading it again gives the same module.
pub struct Hooks {
    library: libloading::Library,
}

impl Hooks {
    pub fn of(module: &std::path::Path) -> Self {
        let file = ow_plugin_fixtures::library_of(module);
        // SAFETY: the fixture's initialisation code is the same that already ran in the host.
        let library = unsafe { libloading::Library::new(&file) }
            .unwrap_or_else(|e| panic!("load {}: {e}", file.display()));
        Self { library }
    }

    /// The user's gesture on a parameter (phase 0 begin, 1 change to the plain `value`, 2 end).
    pub fn simulate_edit(&self, param: u32, value: f64, phase: u32) {
        // SAFETY: the symbol has this signature (see `ow_plugin_fixtures::hooks`).
        unsafe {
            let hook: libloading::Symbol<unsafe extern "C" fn(u32, f64, u32)> = self
                .library
                .get(ow_plugin_fixtures::hooks::SIMULATE_EDIT)
                .unwrap();
            hook(param, value, phase);
        }
    }

    /// The editor asks its host for a new size; true when the host agreed.
    pub fn request_resize(&self, width: u32, height: u32) -> bool {
        // SAFETY: as above.
        unsafe {
            let hook: libloading::Symbol<unsafe extern "C" fn(u32, u32) -> u32> = self
                .library
                .get(ow_plugin_fixtures::hooks::REQUEST_RESIZE)
                .unwrap();
            hook(width, height) == 1
        }
    }

    /// Whether the editor's view is attached to a window, and its size.
    pub fn editor_state(&self) -> ow_plugin_fixtures::hooks::EditorState {
        let mut state = ow_plugin_fixtures::hooks::EditorState::default();
        // SAFETY: as above.
        unsafe {
            let hook: libloading::Symbol<
                unsafe extern "C" fn(*mut ow_plugin_fixtures::hooks::EditorState),
            > = self
                .library
                .get(ow_plugin_fixtures::hooks::EDITOR_STATE)
                .unwrap();
            hook(&mut state);
        }
        state
    }
}

/// Runs one block through `processor` and returns the left and right output.
pub fn run(
    processor: &mut dyn PluginProcessor,
    left: &[f32],
    right: &[f32],
    events: &[Event],
) -> (Vec<f32>, Vec<f32>) {
    let mut l = left.to_vec();
    let mut r = right.to_vec();
    processor.process(&mut [&mut l, &mut r], events);
    (l, r)
}

/// An impulse of 1.0 at frame 0 of `BLOCK` frames of silence.
pub fn impulse() -> Vec<f32> {
    let mut block = vec![0.0; BLOCK];
    block[0] = 1.0;
    block
}

pub fn device(id: DeviceId, kind: &str, params: &[(u32, f64)]) -> Device {
    Device {
        id,
        kind: kind.into(),
        name: kind.into(),
        enabled: true,
        params: params.iter().copied().collect(),
        state: vec![],
        sidechain: None,
    }
}

/// One MIDI track holding a long held note on `instrument`, with `inserts` after it. Returns the
/// ids of the instrument and the inserts.
pub fn add_track(
    editor: &mut Editor,
    instrument: (&str, &[(u32, f64)]),
    inserts: &[(&str, &[(u32, f64)])],
) -> (DeviceId, Vec<DeviceId>) {
    let track: TrackId = editor.alloc_id().unwrap();
    let synth: DeviceId = editor.alloc_id().unwrap();
    let ids: Vec<DeviceId> = inserts.iter().map(|_| editor.alloc_id().unwrap()).collect();
    let clip = editor.alloc_id().unwrap();
    let mut devices = vec![device(synth, instrument.0, instrument.1)];
    devices.extend(
        ids.iter()
            .zip(inserts)
            .map(|(id, (kind, params))| device(*id, kind, params)),
    );
    editor
        .apply(EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned {
                item: Track {
                    id: track,
                    name: "Keys".into(),
                    color: 0,
                    kind: TrackKind::Midi,
                    strip: Strip {
                        inserts: ids.clone(),
                        ..Strip::default()
                    },
                    instrument: Some(synth),
                },
                clips: vec![Clip {
                    id: clip,
                    track,
                    name: "notes".into(),
                    start: 0.0,
                    length: 8.0,
                    muted: false,
                    content: ClipContent::Midi {
                        notes: vec![Note {
                            start: 0.0,
                            length: 8.0,
                            key: 60,
                            velocity: 127,
                        }],
                        controllers: vec![],
                    },
                }],
                devices,
                lanes: vec![],
            }),
        })
        .unwrap();
    (synth, ids)
}

pub fn render(renderer: &mut OfflineRenderer, frames: usize) -> Vec<f32> {
    renderer.render(frames, 512)
}

pub fn player(compiled: ow_engine::Compiled) -> OfflineRenderer {
    let mut renderer = OfflineRenderer::new(compiled.plan, 2);
    assert!(renderer.link().swap_timeline(compiled.timeline).is_ok());
    assert!(renderer.send(Message::Transport(TransportCommand::Play)));
    renderer
}
