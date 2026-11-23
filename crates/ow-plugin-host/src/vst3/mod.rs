//! VST3 hosting on the `vst3` crate's bindings and our own host interfaces.
//!
//! # Threads
//!
//! The same split as CLAP ([`crate::clap`]):
//!
//! * The **main-thread side** ([`Main`]) holds the component, the processor interface and the
//!   controller, which VST3 requires to be used from the main thread. It lives in a thread-local
//!   table on the plugin main thread (see [`crate::executor`]), keyed by a process-wide instance
//!   number, and is only touched by jobs the [`Vst3Plugin`] sends there.
//! * The **audio-thread side** ([`audio::AudioSide`]) holds the processor and the buffers and
//!   lists handed to `process`. It sits behind a mutex the audio thread only `try_lock`s.
//! * [`Vst3Plugin`] is the `Send + Sync` control-side object.
//!
//! # Parameters
//!
//! The project stores plain values (what the controller's `normalizedParamToPlain` maps to);
//! `process` takes normalised ones. [`Vst3Plugin::set_params`] converts through the controller
//! (one main-thread call), tells the controller (so an open editor follows) and queues the
//! normalised value for the processor. Edits made in the plugin's editor arrive through the
//! component handler ([`host::ComponentHandler`]), which does the same toward the processor and
//! reports the plain value to the edit sink.
//!
//! # State
//!
//! The component's state and the controller's state are saved together in one blob (our own
//! container, tagged `vst3` in the project): `OWV3`, then the length-prefixed component state and
//! controller state.

// The SDK's enumeration constants are `c_int` on Windows and `c_uint` elsewhere, so casts that
// are no-ops on one platform are needed on the other.
#![allow(unsafe_code, clippy::unnecessary_cast)]

mod audio;
mod host;
mod module;
mod view;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

use self::audio::{AudioSide, Layout};
use self::host::{ComponentHandler, HostApp, MemStream, Shared, lock, read_wide};
use self::module::parse_cid;
use self::view::EditorState;
use crate::hosted::PluginError;
use crate::info::ParamInfo as ParamRecord;

/// Parameter edits the queue between control and audio thread holds; more are dropped until the
/// audio thread catches up.
const EDIT_QUEUE: usize = 1024;
/// Longest enumeration the scanner reads option names for.
const MAX_CHOICES: usize = 128;
/// Magic of the state container.
const STATE_MAGIC: &[u8; 4] = b"OWV3";

static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Live instances by key (main thread only).
    static INSTANCES: RefCell<HashMap<u64, Main>> = RefCell::new(HashMap::new());
}

// ---------------------------------------------------------------------------------------------
// Main-thread side

/// An instance on the main thread.
struct Main {
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    /// The controller is its own object (not the component answering for it).
    separate_controller: bool,
    connected: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    /// Kept alive: plugins keep the context and the handler.
    _host: ComWrapper<HostApp>,
    _handler: ComWrapper<ComponentHandler>,
    layout: Layout,
    shared: Arc<Shared>,
    editor: Rc<EditorState>,
    active: bool,
    torn_down: bool,
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

/// Creates and initialises the plugin class `id` (32 hex digits) of the module at `path`: its
/// component and controller, connected and with our component handler installed. Not active
/// yet. Main thread only.
fn create_main(path: &Path, id: &str, shared: Arc<Shared>) -> Result<Main, String> {
    let cid = parse_cid(id).ok_or_else(|| format!("{id} is not a VST3 class id"))?;
    let module = module::load(path)?;
    let host = ComWrapper::new(HostApp);
    let context = host
        .to_com_ptr::<FUnknown>()
        .ok_or_else(|| "the host context is not an FUnknown".to_string())?;
    module.set_host_context(context.as_ptr());

    let component = module
        .create::<IComponent>(&cid)
        .ok_or_else(|| "the plugin could not be created".to_string())?;
    // SAFETY: a live component; `initialize` runs the plugin's own set-up.
    if unsafe { component.initialize(context.as_ptr()) } != kResultOk {
        return Err("the plugin refused to initialise".into());
    }
    let mut main = Main {
        processor: component
            .cast::<IAudioProcessor>()
            .ok_or_else(|| "the plugin has no audio processor".to_string())?,
        component,
        controller: None,
        separate_controller: false,
        connected: None,
        _host: host,
        _handler: ComWrapper::new(ComponentHandler::new(Arc::clone(&shared))),
        layout: Layout::default(),
        shared,
        editor: Rc::new(EditorState::default()),
        active: false,
        torn_down: false,
    };
    // From here on a failure drops `main`, which terminates what was initialised.

    // The controller: the component itself, or a class of its own that the component names.
    main.controller = main.component.cast::<IEditController>();
    if main.controller.is_none() {
        let mut class_id: TUID = [0; 16];
        // SAFETY: a live component and a valid out pointer.
        let named = unsafe { main.component.getControllerClassId(&mut class_id) } == kResultOk;
        if named
            && let Some(controller) =
                module.create::<IEditController>(&module::tuid_bytes(&class_id))
        {
            // SAFETY: a live controller.
            if unsafe { controller.initialize(context.as_ptr()) } == kResultOk {
                main.separate_controller = true;
                main.controller = Some(controller);
            }
        }
    }
    if main.separate_controller
        && let Some(controller) = &main.controller
        && let (Some(a), Some(b)) = (
            main.component.cast::<IConnectionPoint>(),
            controller.cast::<IConnectionPoint>(),
        )
    {
        // SAFETY: two live connection points.
        unsafe {
            a.connect(b.as_ptr());
            b.connect(a.as_ptr());
        }
        main.connected = Some((a, b));
    }
    if let Some(controller) = &main.controller {
        let handler = main
            ._handler
            .to_com_ptr::<IComponentHandler>()
            .ok_or_else(|| "the handler is not an IComponentHandler".to_string())?;
        // SAFETY: a live controller and handler.
        unsafe { controller.setComponentHandler(handler.as_ptr()) };
        *lock(&main.shared.controller) = Some(controller.clone());
    }
    Ok(main)
}

impl Main {
    /// Asks for stereo main buses, activates them (and the note input) and reads what the plugin
    /// ended up with. The plugin must be inactive.
    fn setup_buses(&mut self) -> Layout {
        let audio = MediaTypes_::kAudio as MediaType;
        let events = MediaTypes_::kEvent as MediaType;
        let input = BusDirections_::kInput as BusDirection;
        let output = BusDirections_::kOutput as BusDirection;
        // SAFETY: a live component and processor; every pointer is valid for its call.
        unsafe {
            let in_buses = self.component.getBusCount(audio, input).max(0);
            let out_buses = self.component.getBusCount(audio, output).max(0);
            let arrangement = |direction: BusDirection,
                               index: i32,
                               processor: &ComPtr<IAudioProcessor>| {
                let mut current: SpeakerArrangement = 0;
                if index == 0 {
                    SpeakerArr::kStereo
                } else if processor.getBusArrangement(direction, index, &mut current) == kResultOk {
                    current
                } else {
                    0
                }
            };
            let mut ins: Vec<SpeakerArrangement> = (0..in_buses)
                .map(|index| arrangement(input, index, &self.processor))
                .collect();
            let mut outs: Vec<SpeakerArrangement> = (0..out_buses)
                .map(|index| arrangement(output, index, &self.processor))
                .collect();
            // A plugin that cannot do stereo keeps its own arrangement, read back below.
            self.processor.setBusArrangements(
                if ins.is_empty() {
                    std::ptr::null_mut()
                } else {
                    ins.as_mut_ptr()
                },
                in_buses,
                if outs.is_empty() {
                    std::ptr::null_mut()
                } else {
                    outs.as_mut_ptr()
                },
                out_buses,
            );
            let channels =
                |direction: BusDirection, buses: i32, processor: &ComPtr<IAudioProcessor>| {
                    if buses == 0 {
                        return 0;
                    }
                    let mut current: SpeakerArrangement = 0;
                    if processor.getBusArrangement(direction, 0, &mut current) == kResultOk {
                        current.count_ones() as usize
                    } else {
                        0
                    }
                };
            let in_channels = channels(input, in_buses, &self.processor);
            let out_channels = channels(output, out_buses, &self.processor);
            for index in 0..in_buses {
                self.component.activateBus(
                    audio,
                    input,
                    index,
                    u8::from(index == 0 && in_channels > 0),
                );
            }
            for index in 0..out_buses {
                self.component.activateBus(
                    audio,
                    output,
                    index,
                    u8::from(index == 0 && out_channels > 0),
                );
            }
            let event_inputs = self.component.getBusCount(events, input).max(0);
            for index in 0..event_inputs {
                self.component
                    .activateBus(events, input, index, u8::from(index == 0));
            }
            let event_outputs = self.component.getBusCount(events, output).max(0);
            for index in 0..event_outputs {
                self.component.activateBus(events, output, index, 0);
            }
            Layout {
                in_channels,
                out_channels,
                in_buses: in_buses as usize,
                out_buses: out_buses as usize,
                note_input: event_inputs > 0,
            }
        }
    }

    /// Sets the plugin up for `sample_rate` and blocks of up to `max_block` frames and activates
    /// it. Main thread only; the plugin must be inactive.
    fn activate(&mut self, sample_rate: u32, max_block: usize) -> Result<AudioSide, String> {
        self.layout = self.setup_buses();
        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: i32::try_from(max_block).unwrap_or(i32::MAX).max(1),
            sampleRate: f64::from(sample_rate),
        };
        // SAFETY: a live processor and component.
        unsafe {
            let result = self.processor.setupProcessing(&mut setup);
            if result != kResultOk && result != kNotImplemented {
                return Err("the plugin refused its processing set-up".into());
            }
            let result = self.component.setActive(1);
            if result != kResultOk && result != kNotImplemented {
                return Err("the plugin could not be activated".into());
            }
        }
        self.active = true;
        Ok(AudioSide::new(
            self.processor.clone(),
            self.layout,
            max_block.max(1),
            f64::from(sample_rate),
        ))
    }

    /// Stops processing and deactivates the plugin. Main thread only.
    fn deactivate(&mut self, side: Option<AudioSide>) {
        if let Some(mut side) = side {
            side.stop_processing();
        }
        if self.active {
            self.active = false;
            // SAFETY: a live component.
            unsafe { self.component.setActive(0) };
        }
    }

    /// The latency the plugin reports now, in frames.
    fn latency(&self) -> u32 {
        // SAFETY: a live processor.
        unsafe { self.processor.getLatencySamples() }
    }

    /// The parameter list. Main thread only.
    fn params(&self) -> Vec<ParamRecord> {
        let Some(controller) = &self.controller else {
            return Vec::new();
        };
        let mut params = Vec::new();
        // SAFETY: a live controller; the info struct is plain data.
        unsafe {
            for index in 0..controller.getParameterCount().max(0) {
                let mut info: ParameterInfo = std::mem::zeroed();
                if controller.getParameterInfo(index, &mut info) != kResultOk {
                    continue;
                }
                let flags = info.flags;
                let hidden = flags
                    & (ParameterInfo_::ParameterFlags_::kIsHidden
                        | ParameterInfo_::ParameterFlags_::kIsProgramChange)
                        as i32
                    != 0;
                let read_only = flags & ParameterInfo_::ParameterFlags_::kIsReadOnly as i32 != 0;
                let min = controller.normalizedParamToPlain(info.id, 0.0);
                let max = controller.normalizedParamToPlain(info.id, 1.0);
                let default =
                    controller.normalizedParamToPlain(info.id, info.defaultNormalizedValue);
                let steps = info.stepCount.max(0);
                // Whole values `min..=min+steps`: an enumeration or a switch.
                let integral = steps > 0
                    && (min - min.round()).abs() < 1e-9
                    && ((max - min) - f64::from(steps)).abs() < 1e-9;
                let mut choices = Vec::new();
                if integral && (steps as usize) < MAX_CHOICES {
                    for step in 0..=steps {
                        let normalized =
                            controller.plainParamToNormalized(info.id, min + f64::from(step));
                        let mut text: String128 = [0; 128];
                        let label = if controller
                            .getParamStringByValue(info.id, normalized, &mut text)
                            == kResultOk
                        {
                            read_wide(&text)
                        } else {
                            String::new()
                        };
                        choices.push(if label.is_empty() {
                            (min as i64 + i64::from(step)).to_string()
                        } else {
                            label
                        });
                    }
                }
                let title = match read_wide(&info.title) {
                    title if title.is_empty() => read_wide(&info.shortTitle),
                    title => title,
                };
                params.push(ParamRecord {
                    id: info.id,
                    name: title,
                    module: String::new(),
                    min,
                    max,
                    default,
                    stepped: integral,
                    choices,
                    hidden,
                    read_only,
                });
            }
        }
        params
    }

    /// Saves the component's and the controller's state into one blob.
    fn save_state(&self) -> Result<Vec<u8>, PluginError> {
        let component = save_stream(|stream| {
            // SAFETY: a live component and stream.
            unsafe { self.component.getState(stream) }
        })?;
        let controller = match &self.controller {
            Some(controller) => save_stream(|stream| {
                // SAFETY: a live controller and stream.
                unsafe { controller.getState(stream) }
            })
            .unwrap_or_default(),
            None => Vec::new(),
        };
        let mut blob = Vec::with_capacity(12 + component.len() + controller.len());
        blob.extend_from_slice(STATE_MAGIC);
        for part in [&component, &controller] {
            blob.extend_from_slice(&u32::try_from(part.len()).unwrap_or(u32::MAX).to_le_bytes());
            blob.extend_from_slice(part);
        }
        Ok(blob)
    }

    /// Restores a blob written by [`save_state`](Self::save_state).
    fn load_state(&self, blob: &[u8]) -> Result<(), PluginError> {
        let (component, controller_state) = unpack_state(blob)
            .ok_or_else(|| PluginError::Plugin("not an OpenWave VST3 state".into()))?;
        let stream = ComWrapper::new(MemStream::reading(component));
        let stream_ptr = stream
            .to_com_ptr::<IBStream>()
            .ok_or_else(|| PluginError::Plugin("stream".into()))?;
        // SAFETY: a live component and stream.
        let result = unsafe { self.component.setState(stream_ptr.as_ptr()) };
        if result != kResultOk && result != kNotImplemented {
            return Err(PluginError::Plugin(
                "the plugin could not read its state".into(),
            ));
        }
        if let Some(controller) = &self.controller {
            // The controller follows the component's state, then reads its own.
            let again = ComWrapper::new(MemStream::reading(component));
            if let Some(again_ptr) = again.to_com_ptr::<IBStream>() {
                // SAFETY: a live controller and stream.
                unsafe { controller.setComponentState(again_ptr.as_ptr()) };
            }
            if !controller_state.is_empty() {
                let own = ComWrapper::new(MemStream::reading(controller_state));
                if let Some(own_ptr) = own.to_com_ptr::<IBStream>() {
                    // SAFETY: a live controller and stream.
                    unsafe { controller.setState(own_ptr.as_ptr()) };
                }
            }
        }
        Ok(())
    }

    /// Terminates the plugin. Idempotent.
    fn teardown(&mut self) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;
        view::close(&self.editor);
        if self.active {
            self.active = false;
            // SAFETY: a live component.
            unsafe { self.component.setActive(0) };
        }
        // SAFETY: live objects, released in the order the SDK documents.
        unsafe {
            if let Some((a, b)) = self.connected.take() {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(controller) = &self.controller {
                controller.setComponentHandler(std::ptr::null_mut());
            }
        }
        *lock(&self.shared.controller) = None;
        // SAFETY: as above.
        unsafe {
            if self.separate_controller
                && let Some(controller) = &self.controller
            {
                controller.terminate();
            }
            self.component.terminate();
        }
    }
}

impl Drop for Main {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Runs `save` against a fresh memory stream and returns what the plugin wrote.
fn save_stream(save: impl FnOnce(*mut IBStream) -> tresult) -> Result<Vec<u8>, PluginError> {
    let stream = ComWrapper::new(MemStream::default());
    let pointer = stream
        .to_com_ptr::<IBStream>()
        .ok_or_else(|| PluginError::Plugin("stream".into()))?;
    let result = save(pointer.as_ptr());
    if result != kResultOk && result != kNotImplemented {
        return Err(PluginError::Plugin(
            "the plugin could not save its state".into(),
        ));
    }
    Ok(stream.bytes())
}

/// The two parts of a state blob.
fn unpack_state(blob: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = blob.strip_prefix(STATE_MAGIC)?;
    let (length, rest) = rest.split_first_chunk::<4>()?;
    let length = u32::from_le_bytes(*length) as usize;
    let component = rest.get(..length)?;
    let rest = &rest[length..];
    let (length, rest) = rest.split_first_chunk::<4>()?;
    let length = u32::from_le_bytes(*length) as usize;
    let controller = rest.get(..length)?;
    Some((component, controller))
}

mod plugin;

pub use plugin::{Vst3Plugin, describe_file};
