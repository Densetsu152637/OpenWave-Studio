//! CLAP hosting on `clack-host`.
//!
//! # Threads
//!
//! A CLAP plugin instance is split across threads by the format, and so is this module:
//!
//! * The **main-thread side** ([`Main`]) holds the `PluginInstance`, which is not `Send`. It
//!   lives in a thread-local table on the plugin main thread (see [`crate::executor`]), keyed by
//!   a process-wide instance number, and is only touched by jobs the [`ClapPlugin`] sends there.
//! * The **audio-thread side** ([`AudioSide`]) holds the activated audio processor, preallocated
//!   buffers and the consumer end of the parameter queue. It sits behind a mutex
//!   ([`AudioSlot`]) that the audio thread only ever `try_lock`s (a failed attempt, which means
//!   the control side is tearing the instance down, outputs the input unchanged or silence) and
//!   that the control side takes for a few instructions to deactivate or re-activate.
//! * [`ClapPlugin`] is the `Send + Sync` control-side object: it creates both sides, queues
//!   parameter edits on the lock-free ring, and reaches the main-thread side through the
//!   executor.
//!
//! # Realtime
//!
//! Everything [`ClapProcessor::process`] does itself is allocation-free: events are pushed into
//! preallocated buffers, port descriptors reuse preallocated storage, audio is copied between
//! fixed buffers. The call into the plugin is wrapped in `assert_no_alloc::permit_alloc`, so the
//! allocation guard that wraps the engine's callback in debug and test builds checks this crate's
//! code around the plugin and leaves the plugin's own allocations (which we cannot change) alone.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, HostGui, HostGuiImpl, PluginGui, Window as GuiWindow,
};
use clack_extensions::latency::{HostLatency, HostLatencyImpl, PluginLatency};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::note_ports::PluginNotePorts;
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamInfoBuffer,
    ParamInfoFlags, ParamRescanFlags, PluginParams,
};
use clack_extensions::state::{HostState, HostStateImpl, PluginState};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_host::events::Match;
use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent, ParamValueEvent};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::prelude::*;
use ow_dsp::{Event, EventKind};

use crate::editor::{self, EditorWindow, WindowEvent};
use crate::edits::{Edit, EditSender};
use crate::executor::{self, MainThreadExecutor};
use crate::hosted::{
    EditPhase, EditSink, EditorEdit, HostedPlugin, PluginError, PluginProcessor, Requests,
};
use crate::info::{Format, ParamInfo as ParamRecord, PluginInfo, kind_of};

/// Parameter events the queue between control and audio thread holds; more are dropped (the
/// newest first) until the audio thread catches up.
const EDIT_QUEUE: usize = 1024;
/// Events one block hands to a plugin (parameter edits plus notes); the rest are dropped.
const MAX_BLOCK_EVENTS: usize = 2048;
/// Longest enumeration the scanner reads option names for.
const MAX_CHOICES: usize = 128;

static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------------------------
// Host handlers

/// What a plugin asked of the host, noted by its callbacks and served by `service`.
#[derive(Default)]
struct Flags {
    callback: AtomicBool,
    restart: AtomicBool,
    latency: AtomicBool,
    state_dirty: AtomicBool,
    params: AtomicBool,
}

/// The extensions a plugin implements, looked up while it initialises.
#[derive(Clone, Copy, Default)]
struct Extensions {
    params: Option<PluginParams>,
    latency: Option<PluginLatency>,
    state: Option<PluginState>,
    audio_ports: Option<PluginAudioPorts>,
    note_ports: Option<PluginNotePorts>,
    gui: Option<PluginGui>,
}

thread_local! {
    /// True while this thread is inside a plugin's `process` (CLAP `thread-check`).
    static IN_AUDIO: Cell<bool> = const { Cell::new(false) };
    /// Loaded plugin libraries by path (main thread only; libraries are never unloaded).
    static ENTRIES: RefCell<HashMap<PathBuf, PluginEntry>> = RefCell::new(HashMap::new());
    /// Live instances by key (main thread only).
    static INSTANCES: RefCell<HashMap<u64, Main>> = RefCell::new(HashMap::new());
    /// Open editor windows by instance key (main thread only). Separate from `INSTANCES` so a
    /// plugin that asks the host to resize its window from inside one of its own calls (while an
    /// instance is borrowed) finds the window.
    static EDITORS: RefCell<HashMap<u64, ClapEditor>> = RefCell::new(HashMap::new());
}

/// An open CLAP editor: the window the plugin's GUI is embedded in.
struct ClapEditor {
    window: EditorWindow,
    resizable: bool,
}

struct Shared {
    key: u64,
    flags: Arc<Flags>,
    executor: Arc<dyn MainThreadExecutor>,
    extensions: OnceLock<Extensions>,
}

impl<'a> SharedHandler<'a> for Shared {
    fn initializing(&self, instance: InitializingPluginHandle<'a>) {
        let _ = self.extensions.set(Extensions {
            params: instance.get_extension(),
            latency: instance.get_extension(),
            state: instance.get_extension(),
            audio_ports: instance.get_extension(),
            note_ports: instance.get_extension(),
            gui: instance.get_extension(),
        });
    }

    fn request_restart(&self) {
        self.flags.restart.store(true, Ordering::Release);
    }

    fn request_process(&self) {
        // The processor is always running; nothing to wake.
    }

    fn request_callback(&self) {
        self.flags.callback.store(true, Ordering::Release);
    }
}

impl HostThreadCheckImpl for Shared {
    fn is_main_thread(&self) -> bool {
        self.executor.is_main_thread()
    }

    fn is_audio_thread(&self) -> bool {
        IN_AUDIO.with(Cell::get)
    }
}

impl HostLogImpl for Shared {
    fn log(&self, severity: LogSeverity, message: &str) {
        if matches!(severity, LogSeverity::Error | LogSeverity::Fatal) {
            eprintln!("[openwave] plugin: {message}");
        }
    }
}

impl HostGuiImpl for Shared {
    fn resize_hints_changed(&self) {}

    fn request_resize(&self, new_size: GuiSize) -> Result<(), clack_host::host::HostError> {
        let resized = EDITORS.with(|editors| {
            let editors = editors.try_borrow().ok()?;
            let editor = editors.get(&self.key)?;
            editor
                .window
                .set_client_size(new_size.width, new_size.height);
            Some(())
        });
        resized.ok_or(clack_host::host::HostError::Message(
            "the editor window is not open",
        ))
    }

    fn request_show(&self) -> Result<(), clack_host::host::HostError> {
        Ok(())
    }

    fn request_hide(&self) -> Result<(), clack_host::host::HostError> {
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {}
}

impl HostParamsImplShared for Shared {
    fn request_flush(&self) {
        // Parameters are applied by every `process` call, which the engine makes every block.
    }
}

struct MainHandler<'a> {
    shared: &'a Shared,
}

impl<'a> MainThreadHandler<'a> for MainHandler<'a> {}

impl HostLatencyImpl for MainHandler<'_> {
    fn changed(&self) {
        self.shared.flags.latency.store(true, Ordering::Release);
    }
}

impl HostParamsImplMainThread for MainHandler<'_> {
    fn rescan(&self, _flags: ParamRescanFlags) {
        self.shared.flags.params.store(true, Ordering::Release);
    }

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostStateImpl for MainHandler<'_> {
    fn mark_dirty(&self) {
        self.shared.flags.state_dirty.store(true, Ordering::Release);
    }
}

struct AudioHandler;

impl AudioProcessorHandler<'_> for AudioHandler {}

struct ClapHost;

impl HostHandlers for ClapHost {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainHandler<'a>;
    type AudioProcessor<'a> = AudioHandler;

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Shared) {
        builder
            .register::<HostLog>()
            .register::<HostLatency>()
            .register::<HostParams>()
            .register::<HostGui>()
            .register::<HostState>()
            .register::<HostThreadCheck>();
    }
}

// ---------------------------------------------------------------------------------------------
// Main-thread side

/// An instance on the main thread.
struct Main {
    instance: PluginInstance<ClapHost>,
    extensions: Extensions,
    /// The sample rate the plugin was last activated for (bounds its reported latency).
    sample_rate: u32,
}

/// What activation needs and produces.
struct Layout {
    /// Channel counts of the audio input ports, in port order.
    inputs: Vec<usize>,
    /// Channel counts of the audio output ports.
    outputs: Vec<usize>,
    /// True when the plugin has a note input port.
    note_input: bool,
}

/// Loads (or reuses) the library at `path`. Main thread only.
#[allow(unsafe_code)]
fn load_entry(path: &Path) -> Result<PluginEntry, String> {
    if let Some(entry) = ENTRIES.with(|entries| entries.borrow().get(path).cloned()) {
        return Ok(entry);
    }
    // SAFETY: loading a plugin library runs the plugin's initialisation code, which is the whole
    // point of hosting it. Scanning loads candidates in a separate process first, so a library
    // that crashes while loading is found there; a library that passed the scan is trusted to
    // load again (the architecture record accepts in-process hosting for performance).
    let entry = unsafe { PluginEntry::load(path) }
        .map_err(|error| format!("could not load {}: {error}", path.display()))?;
    ENTRIES.with(|entries| {
        entries
            .borrow_mut()
            .insert(path.to_path_buf(), entry.clone())
    });
    Ok(entry)
}

fn host_info() -> Result<HostInfo, String> {
    HostInfo::new(
        "OpenWave Studio",
        "OpenWave Studio",
        "https://github.com/Densetsu152637/OpenWave-Studio",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| error.to_string())
}

/// Creates (without activating) the plugin `id` from the library at `path`. Main thread only.
fn create_instance(
    executor: &Arc<dyn MainThreadExecutor>,
    path: &Path,
    id: &str,
    key: u64,
) -> Result<(Main, Arc<Flags>), String> {
    let entry = load_entry(path)?;
    let id = CString::new(id).map_err(|_| "the plugin id contains a NUL".to_string())?;
    let flags = Arc::new(Flags::default());
    let executor = Arc::clone(executor);
    let shared_flags = Arc::clone(&flags);
    let instance = PluginInstance::<ClapHost>::new(
        move |_| Shared {
            key,
            flags: shared_flags,
            executor,
            extensions: OnceLock::new(),
        },
        |shared| MainHandler { shared },
        &entry,
        &id,
        &host_info()?,
    )
    .map_err(|error| format!("could not create the plugin: {error}"))?;
    let extensions = instance
        .access_shared_handler(|shared| shared.extensions.get().copied().unwrap_or_default());
    let main = Main {
        instance,
        extensions,
        sample_rate: 48_000,
    };
    Ok((main, flags))
}

impl Main {
    /// The audio and note port layout. Main thread only.
    fn layout(&mut self) -> Layout {
        let handle = self.instance.plugin_handle();
        let mut buffer = AudioPortInfoBuffer::new();
        let mut ports = |is_input: bool| -> Vec<usize> {
            let Some(extension) = self.extensions.audio_ports else {
                return Vec::new();
            };
            (0..extension.count(&handle, is_input))
                .map(|index| {
                    extension
                        .get(&handle, index, is_input, &mut buffer)
                        .map_or(0, |info| info.channel_count as usize)
                })
                .collect()
        };
        let inputs = ports(true);
        let outputs = ports(false);
        let note_input = self
            .extensions
            .note_ports
            .is_some_and(|extension| extension.count(&handle, true) > 0);
        Layout {
            inputs,
            outputs,
            note_input,
        }
    }

    /// The parameter list. Main thread only.
    fn params(&mut self) -> Vec<ParamRecord> {
        let Some(extension) = self.extensions.params else {
            return Vec::new();
        };
        let handle = self.instance.plugin_handle();
        let mut buffer = ParamInfoBuffer::new();
        let mut text = [0_u8; 256];
        let mut params = Vec::new();
        for index in 0..extension.count(&handle) {
            let Some(info) = extension.get_info(&handle, index, &mut buffer) else {
                continue;
            };
            let flags = info.flags;
            let stepped = flags.contains(ParamInfoFlags::IS_STEPPED);
            let mut choices = Vec::new();
            if flags.contains(ParamInfoFlags::IS_ENUM)
                && info.min_value >= 0.0
                && info.max_value >= info.min_value
                && ((info.max_value - info.min_value) as usize) < MAX_CHOICES
            {
                let first = info.min_value.round() as i64;
                let last = info.max_value.round() as i64;
                for value in first..=last {
                    let label = extension
                        .value_to_text(&handle, info.id, value as f64, &mut text)
                        .ok()
                        .map(|bytes| {
                            let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
                            String::from_utf8_lossy(&bytes[..end]).into_owned()
                        })
                        .filter(|label| !label.is_empty())
                        .unwrap_or_else(|| value.to_string());
                    choices.push(label);
                }
            }
            params.push(ParamRecord {
                id: info.id.get(),
                name: String::from_utf8_lossy(info.name).into_owned(),
                module: String::from_utf8_lossy(info.module).into_owned(),
                min: info.min_value,
                max: info.max_value,
                default: info.default_value,
                stepped: stepped || !choices.is_empty(),
                choices,
                hidden: flags.contains(ParamInfoFlags::IS_HIDDEN),
                read_only: flags.contains(ParamInfoFlags::IS_READONLY),
            });
        }
        params
    }

    /// The latency the plugin reports now. Main thread only.
    fn latency(&mut self) -> u32 {
        let Some(extension) = self.extensions.latency else {
            return 0;
        };
        crate::hosted::clamp_latency(
            extension.get(&self.instance.plugin_handle()),
            self.sample_rate,
        )
    }

    /// Activates the plugin and builds its audio side. Main thread only.
    fn activate(&mut self, sample_rate: u32, max_block: usize) -> Result<AudioSide, String> {
        self.sample_rate = sample_rate;
        let layout = self.layout();
        let configuration = PluginAudioConfiguration {
            sample_rate: f64::from(sample_rate),
            min_frames_count: 1,
            max_frames_count: u32::try_from(max_block).unwrap_or(u32::MAX).max(1),
        };
        let stopped = self
            .instance
            .activate(|_, _| AudioHandler, configuration)
            .map_err(|error| format!("could not activate the plugin: {error}"))?;
        Ok(AudioSide::new(
            PluginAudioProcessor::from(stopped),
            &layout,
            max_block,
        ))
    }
}

/// Runs `work` on the instance `key`. Main thread only.
fn with_main<T>(key: u64, work: impl FnOnce(&mut Main) -> T) -> Result<T, PluginError> {
    INSTANCES.with(|instances| {
        instances
            .borrow_mut()
            .get_mut(&key)
            .map(work)
            .ok_or(PluginError::Destroyed)
    })
}

// ---------------------------------------------------------------------------------------------
// Editor (CLAP gui extension, embedded in an OpenWave window)

/// The GUI API this platform embeds.
fn gui_api() -> GuiApiType<'static> {
    if cfg!(windows) {
        GuiApiType::WIN32
    } else if cfg!(target_os = "macos") {
        GuiApiType::COCOA
    } else {
        GuiApiType::X11
    }
}

/// The plugin-facing handle of the native view `handle`, when this platform embeds one.
#[allow(unsafe_code)]
fn parent_window(handle: *mut std::ffi::c_void) -> Option<GuiWindow<'static, 'static>> {
    #[cfg(windows)]
    {
        // SAFETY: the window outlives the plugin's GUI (it is destroyed after `gui.destroy`).
        Some(unsafe { GuiWindow::from_win32_hwnd(handle) })
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: as above.
        Some(unsafe { GuiWindow::from_cocoa_nsview(handle) })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = handle;
        None
    }
}

/// Opens the plugin's GUI in a new window, or brings the window forward. Main thread only.
#[allow(unsafe_code)]
fn open_gui(key: u64, main: &mut Main, title: &str) -> Result<(), String> {
    let shown = EDITORS.with(|editors| {
        editors
            .borrow()
            .get(&key)
            .map(|editor| editor.window.show())
    });
    if shown.is_some() {
        return Ok(());
    }
    let gui = main
        .extensions
        .gui
        .ok_or_else(|| "the plugin has no editor".to_string())?;
    let handle = main.instance.plugin_handle();
    let configuration = GuiConfiguration {
        api_type: gui_api(),
        is_floating: false,
    };
    if !gui.is_api_supported(&handle, configuration) {
        return Err("the plugin's editor cannot be embedded on this platform".into());
    }
    gui.create(&handle, configuration)
        .map_err(|error| error.to_string())?;
    // From here on a failure destroys the plugin's GUI again.
    let _ = gui.set_scale(&handle, 1.0);
    let resizable = gui.can_resize(&handle);
    let size = gui.get_size(&handle).unwrap_or(GuiSize {
        width: 480,
        height: 320,
    });
    let window = match EditorWindow::create(title, size.width, size.height, resizable) {
        Ok(window) => window,
        Err(error) => {
            gui.destroy(&handle);
            return Err(error);
        }
    };
    let Some(parent) = parent_window(window.native_handle()) else {
        gui.destroy(&handle);
        return Err("the plugin's editor cannot be embedded on this platform".into());
    };
    // The window is registered first: the plugin may ask for another size while attaching.
    EDITORS.with(|editors| {
        editors
            .borrow_mut()
            .insert(key, ClapEditor { window, resizable });
    });
    // SAFETY: the parent window stays alive until the GUI is destroyed.
    let attached = unsafe { gui.set_parent(&handle, parent) };
    if let Err(error) = attached {
        EDITORS.with(|editors| editors.borrow_mut().remove(&key));
        gui.destroy(&handle);
        return Err(error.to_string());
    }
    let _ = gui.show(&handle);
    EDITORS.with(|editors| {
        if let Some(editor) = editors.borrow().get(&key) {
            editor.window.show();
        }
    });
    Ok(())
}

/// Hides and destroys the plugin's GUI and its window. Idempotent. Main thread only.
// Without editor windows (Linux) the window type has nothing to release.
#[allow(clippy::drop_non_drop)]
fn close_gui(key: u64, main: &mut Main) {
    let editor = EDITORS.with(|editors| editors.borrow_mut().remove(&key));
    let Some(editor) = editor else {
        return;
    };
    if let Some(gui) = main.extensions.gui {
        let handle = main.instance.plugin_handle();
        let _ = gui.hide(&handle);
        gui.destroy(&handle);
    }
    drop(editor);
}

/// Handles what the user did to the editor window. Returns false once the editor is closed.
fn poll_gui(key: u64, main: &mut Main) -> bool {
    let polled = EDITORS.with(|editors| {
        editors
            .borrow()
            .get(&key)
            .map(|editor| (editor.window.poll(), editor.resizable))
    });
    let Some((events, resizable)) = polled else {
        return false;
    };
    for event in events {
        match event {
            WindowEvent::CloseRequested => {
                close_gui(key, main);
                return false;
            }
            WindowEvent::Resized { width, height } if resizable => {
                let Some(gui) = main.extensions.gui else {
                    continue;
                };
                let handle = main.instance.plugin_handle();
                let wanted = GuiSize { width, height };
                let accepted = gui.adjust_size(&handle, wanted).unwrap_or(wanted);
                if gui.set_size(&handle, accepted).is_ok() && accepted != wanted {
                    EDITORS.with(|editors| {
                        if let Some(editor) = editors.borrow().get(&key) {
                            editor
                                .window
                                .set_client_size(accepted.width, accepted.height);
                        }
                    });
                }
            }
            WindowEvent::Resized { .. } => {}
        }
    }
    true
}

// ---------------------------------------------------------------------------------------------
// Audio-thread side

/// Reports the plugin made in one block that the queue holds before the control side serves them.
const REPORT_QUEUE: usize = 1024;

struct AudioSide {
    processor: PluginAudioProcessor<ClapHost>,
    input_events: EventBuffer,
    output_events: EventBuffer,
    input_ports: AudioPorts,
    output_ports: AudioPorts,
    /// Channels of each input port, then of each output port.
    input_layout: Vec<usize>,
    output_layout: Vec<usize>,
    /// One buffer per channel, `max_block` frames each, port after port.
    input_bufs: Vec<Vec<f32>>,
    output_bufs: Vec<Vec<f32>>,
    steady: u64,
    failed: bool,
}

impl AudioSide {
    fn new(processor: PluginAudioProcessor<ClapHost>, layout: &Layout, max_block: usize) -> Self {
        let input_total: usize = layout.inputs.iter().sum();
        let output_total: usize = layout.outputs.iter().sum();
        Self {
            processor,
            input_events: EventBuffer::with_capacity(MAX_BLOCK_EVENTS),
            output_events: EventBuffer::with_capacity(MAX_BLOCK_EVENTS),
            input_ports: AudioPorts::with_capacity(input_total.max(1), layout.inputs.len().max(1)),
            output_ports: AudioPorts::with_capacity(
                output_total.max(1),
                layout.outputs.len().max(1),
            ),
            input_layout: layout.inputs.clone(),
            output_layout: layout.outputs.clone(),
            input_bufs: vec![vec![0.0; max_block]; input_total],
            output_bufs: vec![vec![0.0; max_block]; output_total],
            steady: 0,
            failed: false,
        }
    }
}

/// The audio side and the edits that feed it. Locked briefly by the control side; only
/// `try_lock`ed by the audio thread.
struct AudioSlot {
    side: Option<AudioSide>,
    edits: rtrb::Consumer<Edit>,
    /// Parameter changes the plugin reported while processing (its editor's gestures).
    reports: rtrb::Producer<EditorEdit>,
}

type SharedSlot = Arc<Mutex<AudioSlot>>;

/// The processor a device node owns.
struct ClapProcessor {
    slot: SharedSlot,
    latency: Arc<AtomicU32>,
    /// True for a plugin that makes sound from notes: it is silent when a block is skipped.
    instrument: bool,
}

impl ClapProcessor {
    /// Runs the plugin on `audio`. Returns false when the block was not processed.
    fn run(slot: &mut AudioSlot, audio: &mut [&mut [f32]; 2], events: &[Event]) -> bool {
        let AudioSlot {
            side,
            edits,
            reports,
        } = slot;
        let Some(side) = side.as_mut() else {
            return false;
        };
        let frames = audio[0].len().min(audio[1].len());
        if side.failed || frames == 0 {
            return false;
        }
        let max_block = side.output_bufs.first().map_or(frames, Vec::len);
        let max_block = max_block.max(side.input_bufs.first().map_or(0, Vec::len));
        let frames = frames.min(max_block);

        // Events: queued parameter edits first (at the start of the block), then notes.
        side.input_events.clear();
        side.output_events.clear();
        let mut queued = 0;
        while let Ok(edit) = edits.pop() {
            if queued < MAX_BLOCK_EVENTS / 2
                && let Some(id) = ClapId::from_raw(edit.id)
            {
                side.input_events
                    .push(&ParamValueEvent::new(0, id, Pckn::match_all(), edit.value));
                queued += 1;
            }
        }
        for event in events.iter().take(MAX_BLOCK_EVENTS / 2) {
            let time = event.offset.min(u32::try_from(frames - 1).unwrap_or(0));
            match event.kind {
                EventKind::NoteOn { key, velocity } => {
                    side.input_events.push(&NoteOnEvent::new(
                        time,
                        Pckn::new(0u16, 0u16, u16::from(key), Match::All),
                        f64::from(velocity) / 127.0,
                    ));
                }
                EventKind::NoteOff { key } => {
                    side.input_events.push(&NoteOffEvent::new(
                        time,
                        Pckn::new(0u16, 0u16, u16::from(key), Match::All),
                        0.0,
                    ));
                }
            }
        }

        // Input: the first port carries the signal (mono plugins get the sum), the rest silence.
        let main_in = side.input_layout.first().copied().unwrap_or(0);
        match main_in {
            0 => {}
            1 => {
                for (index, out) in side.input_bufs[0][..frames].iter_mut().enumerate() {
                    *out = 0.5 * (audio[0][index] + audio[1][index]);
                }
            }
            _ => {
                side.input_bufs[0][..frames].copy_from_slice(&audio[0][..frames]);
                side.input_bufs[1][..frames].copy_from_slice(&audio[1][..frames]);
            }
        }

        let AudioSide {
            processor,
            input_events,
            output_events,
            input_ports,
            output_ports,
            input_layout,
            output_layout,
            input_bufs,
            output_bufs,
            steady,
            failed,
        } = side;

        let started = match processor.ensure_processing_started() {
            Ok(started) => started,
            Err(_) => {
                *failed = true;
                return false;
            }
        };

        let mut rest: &mut [Vec<f32>] = input_bufs.as_mut_slice();
        let input_audio = input_ports.with_input_buffers(input_layout.iter().map(|&count| {
            let (head, tail) = std::mem::take(&mut rest).split_at_mut(count);
            rest = tail;
            AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(
                    head.iter_mut()
                        .map(|buffer| InputChannel::variable(&mut buffer[..frames])),
                ),
            }
        }));
        let mut rest: &mut [Vec<f32>] = output_bufs.as_mut_slice();
        let mut output_audio =
            output_ports.with_output_buffers(output_layout.iter().map(|&count| {
                let (head, tail) = std::mem::take(&mut rest).split_at_mut(count);
                rest = tail;
                AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        head.iter_mut().map(|buffer| &mut buffer[..frames]),
                    ),
                }
            }));
        let input_view = input_events.as_input();
        let mut output_view = output_events.as_output();

        IN_AUDIO.with(|flag| flag.set(true));
        let result = assert_no_alloc::permit_alloc(|| {
            started.process(
                &input_audio,
                &mut output_audio,
                &input_view,
                &mut output_view,
                Some(*steady),
                None,
            )
        });
        IN_AUDIO.with(|flag| flag.set(false));
        // What the plugin's editor did during the block (gestures and value changes).
        // (`output_view`, the plugin's window onto this list, is no longer used.)
        for event in output_events.iter() {
            let report = match event.as_core_event() {
                Some(CoreEventSpace::ParamValue(change)) => change
                    .param_id()
                    .map(|id| (id, EditPhase::Change, change.value())),
                Some(CoreEventSpace::ParamGestureBegin(gesture)) => gesture
                    .param_id()
                    .map(|id| (id, EditPhase::Begin, f64::NAN)),
                Some(CoreEventSpace::ParamGestureEnd(gesture)) => {
                    gesture.param_id().map(|id| (id, EditPhase::End, f64::NAN))
                }
                _ => None,
            };
            if let Some((id, phase, value)) = report {
                let _ = reports.push(EditorEdit {
                    param: id.get(),
                    phase,
                    value,
                });
            }
        }
        *steady += frames as u64;
        if result.is_err() {
            *failed = true;
            return false;
        }

        // Output: the first port's first channels; a mono plugin plays on both sides.
        match output_layout.first().copied().unwrap_or(0) {
            0 => {
                audio[0][..frames].fill(0.0);
                audio[1][..frames].fill(0.0);
            }
            1 => {
                audio[0][..frames].copy_from_slice(&output_bufs[0][..frames]);
                audio[1][..frames].copy_from_slice(&output_bufs[0][..frames]);
            }
            _ => {
                audio[0][..frames].copy_from_slice(&output_bufs[0][..frames]);
                audio[1][..frames].copy_from_slice(&output_bufs[1][..frames]);
            }
        }
        true
    }
}

impl PluginProcessor for ClapProcessor {
    fn process(&mut self, audio: &mut [&mut [f32]; 2], events: &[Event]) {
        // A held lock means the control side is tearing the instance down: skip the block
        // (an effect passes its input through, an instrument is silent).
        let processed = match self.slot.try_lock() {
            Ok(mut slot) => Self::run(&mut slot, audio, events),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                Self::run(&mut poisoned.into_inner(), audio, events)
            }
            Err(std::sync::TryLockError::WouldBlock) => false,
        };
        if !processed && self.instrument {
            audio[0].fill(0.0);
            audio[1].fill(0.0);
        }
    }

    fn reset(&mut self) {
        if let Ok(mut slot) = self.slot.try_lock()
            && let Some(side) = slot.side.as_mut()
        {
            side.processor.reset();
        }
    }

    fn latency_frames(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------------------------
// Control side

/// The caller gave up waiting for the creation job. The job may still run (or already ran) on the
/// main thread; a cleanup job queued behind it releases whatever it made.
fn abandon(
    executor: &Arc<dyn MainThreadExecutor>,
    key: u64,
    handoff: Arc<Mutex<Option<AudioSide>>>,
) {
    let _ = executor.dispatch(Box::new(move || {
        let main = INSTANCES.with(|instances| instances.borrow_mut().remove(&key));
        let side = lock(&handoff).take();
        if let (Some(mut main), Some(side)) = (main, side) {
            main.instance.deactivate(side.processor.into_stopped());
        }
    }));
}

/// One CLAP plugin instance.
pub struct ClapPlugin {
    key: u64,
    info: PluginInfo,
    executor: Arc<dyn MainThreadExecutor>,
    slot: SharedSlot,
    edits: Mutex<EditSender>,
    /// Parameters whose editor gesture has begun and not ended, and the last value reported.
    gestures: Mutex<HashSet<u32>>,
    last_reported: Mutex<HashMap<u32, f64>>,
    /// The values this host last sent (or the editor last reported), by parameter id.
    applied: Mutex<HashMap<u32, f64>>,
    /// What the plugin's editor did, as the audio thread saw it.
    reports: Mutex<rtrb::Consumer<EditorEdit>>,
    sink: Mutex<Option<EditSink>>,
    editor_open: Arc<AtomicBool>,
    flags: Arc<Flags>,
    latency: Arc<AtomicU32>,
    /// (sample rate, max block) the instance is activated for.
    config: Mutex<(u32, usize)>,
    destroyed: AtomicBool,
}

impl ClapPlugin {
    /// Creates and activates the plugin `info` describes, restoring `state` when given.
    pub fn create(
        executor: Arc<dyn MainThreadExecutor>,
        info: PluginInfo,
        sample_rate: u32,
        max_block: usize,
        state: Option<Vec<u8>>,
    ) -> Result<Arc<Self>, PluginError> {
        let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
        let (producer, consumer) = rtrb::RingBuffer::new(EDIT_QUEUE);
        let (report_producer, report_consumer) = rtrb::RingBuffer::new(REPORT_QUEUE);
        let path = PathBuf::from(&info.path);
        let id = info.id.clone();
        // The audio side travels through a slot, not the answer: when the caller gives up
        // waiting, the job (which may still run later) leaves its instance for `abandon`.
        let handoff: Arc<Mutex<Option<AudioSide>>> = Arc::new(Mutex::new(None));
        let created = {
            let executor = Arc::clone(&executor);
            let handoff = Arc::clone(&handoff);
            executor::call(executor.clone().as_ref(), move || {
                let (mut main, flags) = create_instance(&executor, &path, &id, key)?;
                if let (Some(state), Some(extension)) = (&state, main.extensions.state) {
                    // A state the plugin cannot read leaves it at its defaults; not fatal.
                    let mut reader: &[u8] = state;
                    let _ = extension.load(&main.instance.plugin_handle(), &mut reader);
                }
                let side = main.activate(sample_rate, max_block)?;
                let latency = main.latency();
                *lock(&handoff) = Some(side);
                INSTANCES.with(|instances| instances.borrow_mut().insert(key, main));
                Ok::<_, String>((flags, latency))
            })
        };
        let (flags, latency) = match created {
            Ok(created) => created.map_err(PluginError::Plugin)?,
            Err(error) => {
                abandon(&executor, key, handoff);
                return Err(error.into());
            }
        };
        let side = lock(&handoff)
            .take()
            .ok_or_else(|| PluginError::Plugin("the plugin instance went missing".into()))?;
        let slot = Arc::new(Mutex::new(AudioSlot {
            side: Some(side),
            edits: consumer,
            reports: report_producer,
        }));
        // On macOS the standard mutex allocates its pthread mutex the first time it is locked.
        // Lock it once here, on the control thread, so the audio thread's first `try_lock`
        // allocates nothing.
        drop(lock(&slot));
        Ok(Arc::new(Self {
            key,
            info,
            executor,
            slot,
            edits: Mutex::new(EditSender::new(producer)),
            gestures: Mutex::new(HashSet::new()),
            last_reported: Mutex::new(HashMap::new()),
            applied: Mutex::new(HashMap::new()),
            reports: Mutex::new(report_consumer),
            sink: Mutex::new(None),
            editor_open: Arc::new(AtomicBool::new(false)),
            flags,
            latency: Arc::new(AtomicU32::new(latency)),
            config: Mutex::new((sample_rate, max_block)),
            destroyed: AtomicBool::new(false),
        }))
    }

    /// Hands the sink what the plugin's editor did since the last call. No main-thread call.
    fn serve_reports(&self) {
        let mut reports = Vec::new();
        {
            let mut consumer = lock(&self.reports);
            while let Ok(report) = consumer.pop() {
                reports.push(report);
            }
        }
        if reports.is_empty() {
            return;
        }
        let sink = lock(&self.sink).clone();
        for mut report in reports {
            match report.phase {
                EditPhase::Change => {
                    lock(&self.last_reported).insert(report.param, report.value);
                }
                EditPhase::Begin => {
                    if !lock(&self.gestures).insert(report.param) {
                        continue;
                    }
                    report.value = self.known_value(report.param);
                }
                EditPhase::End => {
                    if !lock(&self.gestures).remove(&report.param) {
                        continue;
                    }
                    report.value = self.known_value(report.param);
                }
            }
            if let Some(sink) = &sink {
                sink(report);
            }
        }
    }

    /// The newest value of `param` the editor reported, else what the host sent, else its default.
    fn known_value(&self, param: u32) -> f64 {
        lock(&self.last_reported)
            .get(&param)
            .copied()
            .or_else(|| lock(&self.applied).get(&param).copied())
            .or_else(|| self.info.param(param).map(|p| p.default))
            .unwrap_or(0.0)
    }

    /// Ends every gesture that is still open (the editor closed, the plugin is released, the
    /// project was replaced), so the app closes the undo step it opened for it.
    fn close_gestures(&self) {
        self.serve_reports();
        let open: Vec<u32> = lock(&self.gestures).drain().collect();
        let sink = lock(&self.sink).clone();
        for param in open {
            if let Some(sink) = &sink {
                sink(EditorEdit {
                    param,
                    phase: EditPhase::End,
                    value: self.known_value(param),
                });
            }
        }
    }

    fn queue(&self, id: u32, value: f64) -> bool {
        lock(&self.edits).send(Edit { id, value });
        true
    }

    fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, PluginError> {
        if self.destroyed.load(Ordering::Acquire) {
            return Err(PluginError::Destroyed);
        }
        Ok(executor::call(self.executor.as_ref(), work)?)
    }
}

impl HostedPlugin for ClapPlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn processor(&self) -> Box<dyn PluginProcessor> {
        Box::new(ClapProcessor {
            slot: Arc::clone(&self.slot),
            latency: Arc::clone(&self.latency),
            instrument: self.info.instrument,
        })
    }

    fn latency_frames(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    fn set_params(&self, params: &BTreeMap<u32, f64>) -> usize {
        let mut applied = lock(&self.applied);
        let mut queued = 0;
        for param in &self.info.params {
            if param.read_only {
                continue;
            }
            let wanted = match params.get(&param.id) {
                Some(value) => Some(param.clamp(*value)),
                // Set before, unset now: back to the default.
                None if applied.contains_key(&param.id) => Some(param.default),
                None => None,
            };
            let Some(wanted) = wanted else {
                continue;
            };
            if applied.get(&param.id) == Some(&wanted) {
                continue;
            }
            if self.queue(param.id, wanted) {
                applied.insert(param.id, wanted);
                queued += 1;
            }
        }
        // Unset parameters that are back at their default need no memory.
        applied.retain(|id, value| {
            params.contains_key(id) || self.info.param(*id).is_none_or(|p| p.default != *value)
        });
        queued
    }

    fn param_value(&self, id: u32) -> Result<Option<f64>, PluginError> {
        let key = self.key;
        self.call(move || {
            with_main(key, |main| {
                let extension = main.extensions.params?;
                let id = ClapId::from_raw(id)?;
                extension.get_value(&main.instance.plugin_handle(), id)
            })
        })?
    }

    fn save_state(&self) -> Result<Vec<u8>, PluginError> {
        let key = self.key;
        self.call(move || {
            with_main(key, |main| {
                let extension = main.extensions.state.ok_or_else(|| {
                    PluginError::Plugin("the plugin has no saveable state".into())
                })?;
                let mut bytes = Vec::new();
                extension
                    .save(&main.instance.plugin_handle(), &mut bytes)
                    .map_err(|error| PluginError::Plugin(error.to_string()))?;
                Ok(bytes)
            })?
        })?
    }

    fn load_state(&self, state: &[u8]) -> Result<(), PluginError> {
        let key = self.key;
        let state = state.to_vec();
        self.call(move || {
            with_main(key, |main| {
                let extension = main.extensions.state.ok_or_else(|| {
                    PluginError::Plugin("the plugin has no loadable state".into())
                })?;
                let mut reader: &[u8] = &state;
                extension
                    .load(&main.instance.plugin_handle(), &mut reader)
                    .map_err(|error| PluginError::Plugin(error.to_string()))
            })?
        })?
    }

    fn service(&self) -> Requests {
        if self.destroyed.load(Ordering::Acquire) {
            return Requests::default();
        }
        self.serve_reports();
        // Values that waited for room in the edit queue go in now.
        lock(&self.edits).flush();
        let editor = self.editor_open.load(Ordering::Acquire);
        let flags = Arc::clone(&self.flags);
        let pending = [
            &flags.callback,
            &flags.restart,
            &flags.latency,
            &flags.state_dirty,
            &flags.params,
        ]
        .iter()
        .any(|flag| flag.load(Ordering::Acquire))
            || editor;
        if !pending {
            // Nothing was asked: no main-thread call.
            return Requests::default();
        }
        let key = self.key;
        let latency = Arc::clone(&self.latency);
        let restart = flags.restart.swap(false, Ordering::AcqRel);
        let config = *lock(&self.config);
        let slot = Arc::clone(&self.slot);
        let editor_open = Arc::clone(&self.editor_open);
        let closed_by_user = Arc::new(AtomicBool::new(false));
        let closed_flag = Arc::clone(&closed_by_user);
        let served = self.call(move || {
            let mut requests = Requests::default();
            let _ = with_main(key, |main| {
                if editor && !poll_gui(key, main) {
                    editor_open.store(false, Ordering::Release);
                    closed_flag.store(true, Ordering::Release);
                }
                if flags.callback.swap(false, Ordering::AcqRel) {
                    main.instance.call_on_main_thread_callback();
                }
                if restart {
                    // The plugin wants to be re-activated (its ports or latency changed).
                    let mut guard = lock(&slot);
                    if let Some(old) = guard.side.take() {
                        main.instance.deactivate(old.processor.into_stopped());
                    }
                    if let Ok(side) = main.activate(config.0, config.1) {
                        guard.side = Some(side);
                    }
                    drop(guard);
                    flags.latency.store(true, Ordering::Release);
                }
                if flags.latency.swap(false, Ordering::AcqRel) {
                    let reported = main.latency();
                    if latency.swap(reported, Ordering::AcqRel) != reported {
                        requests.latency_changed = true;
                    }
                }
                requests.state_dirty = flags.state_dirty.swap(false, Ordering::AcqRel);
                requests.params_rescanned = flags.params.swap(false, Ordering::AcqRel);
            });
            requests
        });
        if closed_by_user.load(Ordering::Acquire) {
            self.close_gestures();
        }
        served.unwrap_or_default()
    }

    fn configure(&self, sample_rate: u32, max_block: usize) -> Result<(), PluginError> {
        {
            let mut config = lock(&self.config);
            if *config == (sample_rate, max_block) {
                return Ok(());
            }
            *config = (sample_rate, max_block);
        }
        let key = self.key;
        let slot = Arc::clone(&self.slot);
        let latency = Arc::clone(&self.latency);
        self.call(move || {
            with_main(key, |main| {
                let mut guard = lock(&slot);
                if let Some(old) = guard.side.take() {
                    main.instance.deactivate(old.processor.into_stopped());
                }
                let side = main
                    .activate(sample_rate, max_block)
                    .map_err(PluginError::Plugin)?;
                guard.side = Some(side);
                drop(guard);
                latency.store(main.latency(), Ordering::Release);
                Ok(())
            })?
        })?
    }

    fn note_applied(&self, param: u32, value: f64) {
        lock(&self.applied).insert(param, value);
    }

    fn set_edit_sink(&self, sink: EditSink) {
        *lock(&self.sink) = Some(sink);
    }

    fn open_editor(&self) -> Result<(), PluginError> {
        if !editor::AVAILABLE {
            return Err(PluginError::Plugin(
                "plugin editor windows are not available on this platform".into(),
            ));
        }
        let key = self.key;
        let title = self.info.name.clone();
        self.call(move || {
            with_main(key, |main| open_gui(key, main, &title))?.map_err(PluginError::Plugin)
        })??;
        self.editor_open.store(true, Ordering::Release);
        Ok(())
    }

    fn close_editor(&self) {
        let key = self.key;
        let _ = self.call(move || {
            let _ = with_main(key, |main| close_gui(key, main));
        });
        self.editor_open.store(false, Ordering::Release);
        self.close_gestures();
    }

    fn editor_open(&self) -> bool {
        self.editor_open.load(Ordering::Acquire)
    }

    fn editor_size(&self) -> Option<(u32, u32)> {
        let key = self.key;
        self.call(move || {
            EDITORS.with(|editors| {
                editors
                    .borrow()
                    .get(&key)
                    .map(|editor| editor.window.client_size())
            })
        })
        .ok()
        .flatten()
    }

    fn destroy(&self) {
        if self.destroyed.swap(true, Ordering::AcqRel) {
            return;
        }
        let key = self.key;
        let slot = Arc::clone(&self.slot);
        let torn_down = executor::call(self.executor.as_ref(), move || {
            let main = INSTANCES.with(|instances| instances.borrow_mut().remove(&key));
            // Taking the audio side first makes any node that still runs output silence.
            let side = lock(&slot).side.take();
            if let (Some(mut main), Some(side)) = (main, side) {
                close_gui(key, &mut main);
                main.instance.deactivate(side.processor.into_stopped());
            }
        });
        self.editor_open.store(false, Ordering::Release);
        self.close_gestures();
        if torn_down.is_err() {
            eprintln!(
                "[openwave] a plugin instance could not be released: the main thread is gone"
            );
        }
    }
}

impl Drop for ClapPlugin {
    fn drop(&mut self) {
        self.destroy();
    }
}

// ---------------------------------------------------------------------------------------------
// Scanning

/// Lists the plugins in the library at `path`, instantiating each one to read its ports,
/// parameters and latency. Runs on the calling thread, which must be the executor's main thread
/// (the scanner child uses an [`InlineExecutor`](crate::executor::InlineExecutor)). A plugin that
/// crashes here takes the process with it: scan in a child.
pub fn describe_file(
    executor: &Arc<dyn MainThreadExecutor>,
    path: &Path,
) -> Result<Vec<PluginInfo>, String> {
    let entry = load_entry(path)?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| "the library has no CLAP plugin factory".to_string())?;
    let mut found = Vec::new();
    for descriptor in factory.plugin_descriptors() {
        let Some(id) = descriptor.id().and_then(|id| id.to_str().ok()) else {
            continue;
        };
        let text = |value: Option<&std::ffi::CStr>| {
            value
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let features: Vec<String> = descriptor
            .features()
            .map(|feature| feature.to_string_lossy().into_owned())
            .collect();
        let name = text(descriptor.name());
        let vendor = text(descriptor.vendor());
        let version = text(descriptor.version());
        let (mut main, _) = create_instance(executor, path, id, 0)?;
        let layout = main.layout();
        let params = main.params();
        let mut latency = 0;
        if let Ok(side) = main.activate(48_000, 512) {
            latency = main.latency();
            let stopped = side.processor.into_stopped();
            main.instance.deactivate(stopped);
        }
        let instrument = features.iter().any(|feature| feature == "instrument");
        let editor = main.extensions.gui.is_some_and(|gui| {
            gui.is_api_supported(
                &main.instance.plugin_handle(),
                GuiConfiguration {
                    api_type: gui_api(),
                    is_floating: false,
                },
            )
        });
        found.push(PluginInfo {
            format: Format::Clap,
            kind: kind_of(Format::Clap, id),
            id: id.to_string(),
            name: if name.is_empty() {
                id.to_string()
            } else {
                name
            },
            vendor,
            version,
            features,
            instrument,
            audio_inputs: layout.inputs.first().map_or(0, |n| *n as u32),
            audio_outputs: layout.outputs.first().map_or(0, |n| *n as u32),
            note_input: layout.note_input,
            latency,
            params,
            editor,
            path: std::path::absolute(path)
                .unwrap_or_else(|_| path.to_path_buf())
                .to_string_lossy()
                .into_owned(),
        });
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{SlowExecutor, ThreadExecutor};

    /// Creation that outlasts the caller's patience leaves no instance behind.
    #[test]
    fn an_instance_created_after_the_caller_gave_up_is_released() {
        let infos = crate::scan::scan_in_process(&ow_plugin_fixtures::gain_path()).unwrap();
        let info = infos.into_iter().find(|i| !i.instrument).unwrap();
        let inner = Arc::new(ThreadExecutor::new());
        let slow: Arc<dyn MainThreadExecutor> = Arc::new(SlowExecutor {
            inner: Arc::clone(&inner),
            delay: std::time::Duration::from_millis(400),
            timeout: std::time::Duration::from_millis(100),
        });
        let before =
            executor::call(inner.as_ref(), || INSTANCES.with(|i| i.borrow().len())).unwrap();
        let result = ClapPlugin::create(slow, info, 48_000, 256, None);
        assert!(matches!(
            result,
            Err(PluginError::Executor(
                crate::executor::ExecutorError::TimedOut
            ))
        ));
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let after =
            executor::call(inner.as_ref(), || INSTANCES.with(|i| i.borrow().len())).unwrap();
        assert_eq!(after, before, "the late instance was destroyed");
    }
}
