//! The gain effect and the tone instrument.
//!
//! Gain: stereo in, stereo out; `Gain` (id 1, 0 to 2) and `Polarity` (id 2, an enumeration);
//! the output is delayed by [`crate::GAIN_LATENCY`] frames and the plugin reports that latency.
//! State is a small binary blob holding both parameters. Tone: no audio input, one note input
//! port; while a note is held it outputs a constant level `Level * velocity`, which makes the
//! host's note events easy to assert on.
//!
//! The gain effect also has a minimal editor (the CLAP `gui` extension, embedded): a plain native
//! child view that fills the host's window (see [`crate::view`]). The hooks at the end of the
//! file let host tests act as the editor's user (`ow_fixture_simulate_edit` reports a parameter
//! gesture as output events, `ow_fixture_request_resize` asks the host for a new size).

use std::cell::RefCell;
use std::ffi::CStr;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, HostGui, PluginGui, PluginGuiImpl, Window,
};
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::clack_export_entry;
use clack_plugin::entry::prelude::*;
use clack_plugin::events::event_types::{
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::host::HostSharedHandle;
use clack_plugin::plugin::features;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;

use crate::hooks::{EditorState, PHASE_BEGIN, PHASE_CHANGE};
use crate::view::{ChildView, DEFAULT_SIZE};
use crate::{GAIN_ID, GAIN_LATENCY, PARAM_GAIN, PARAM_LEVEL, PARAM_POLARITY, TONE_ID};

const LATENCY: usize = GAIN_LATENCY as usize;
const STATE_MAGIC: &[u8; 4] = b"OWFX";
const STATE_LEN: usize = 13;

// ---------------------------------------------------------------------------------------------
// Entry: one library, two plugins.

pub struct FixtureEntry {
    factory: PluginFactoryWrapper<FixtureFactory>,
}

impl Entry for FixtureEntry {
    fn new(_bundle_path: Option<&CStr>) -> Result<Self, EntryLoadError> {
        Ok(Self {
            factory: PluginFactoryWrapper::new(FixtureFactory {
                gain: PluginDescriptor::new(GAIN_ID, "OpenWave Fixture Gain")
                    .with_vendor("OpenWave Studio tests")
                    .with_version("1.0.0")
                    .with_features([features::AUDIO_EFFECT, features::UTILITY]),
                tone: PluginDescriptor::new(TONE_ID, "OpenWave Fixture Tone")
                    .with_vendor("OpenWave Studio tests")
                    .with_version("1.0.0")
                    .with_features([features::INSTRUMENT, features::SYNTHESIZER]),
            }),
        })
    }

    fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
        builder.register_factory(&self.factory);
    }
}

pub struct FixtureFactory {
    gain: PluginDescriptor,
    tone: PluginDescriptor,
}

impl PluginFactoryImpl for FixtureFactory {
    fn plugin_count(&self) -> u32 {
        2
    }

    fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
        match index {
            0 => Some(&self.gain),
            1 => Some(&self.tone),
            _ => None,
        }
    }

    fn create_plugin<'a>(
        &'a self,
        host_info: HostInfo<'a>,
        plugin_id: &CStr,
    ) -> Option<PluginInstance<'a>> {
        if plugin_id == self.gain.id().unwrap_or_default() {
            let host_gui = host_info.get_extension::<HostGui>();
            Some(PluginInstance::new::<Gain>(
                host_info,
                &self.gain,
                |_| Ok(GainShared::default()),
                move |host, shared| {
                    Ok(GainMain {
                        shared,
                        host: host.shared(),
                        host_gui,
                        gui: RefCell::new(GuiState {
                            view: None,
                            size: DEFAULT_SIZE,
                        }),
                    })
                },
            ))
        } else if plugin_id == self.tone.id().unwrap_or_default() {
            Some(PluginInstance::new::<Tone>(
                host_info,
                &self.tone,
                |_| Ok(ToneShared::default()),
                |_, shared| Ok(ToneMain { shared }),
            ))
        } else {
            None
        }
    }
}

clack_export_entry!(FixtureEntry);

fn stereo_port(writer: &mut AudioPortInfoWriter) {
    writer.set(&AudioPortInfo {
        id: ClapId::new(0),
        name: b"main",
        channel_count: 2,
        flags: AudioPortFlags::IS_MAIN,
        port_type: Some(AudioPortType::STEREO),
        in_place_pair: None,
    });
}

// ---------------------------------------------------------------------------------------------
// Gain

pub struct Gain;

#[derive(Default)]
pub struct GainShared {
    gain_bits: AtomicU64,
    inverted: AtomicU32,
    initialised: AtomicU32,
}

impl GainShared {
    fn gain(&self) -> f64 {
        if self.initialised.load(Ordering::Relaxed) == 0 {
            1.0
        } else {
            f64::from_bits(self.gain_bits.load(Ordering::Relaxed))
        }
    }

    fn set(&self, id: u32, value: f64) {
        match id {
            PARAM_GAIN => {
                self.gain_bits
                    .store(value.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
                self.initialised.store(1, Ordering::Relaxed);
            }
            PARAM_POLARITY => self
                .inverted
                .store(u32::from(value >= 0.5), Ordering::Relaxed),
            _ => {}
        }
    }

    fn handle_events(&self, events: &InputEvents) {
        for event in events {
            if let Some(CoreEventSpace::ParamValue(change)) = event.as_core_event()
                && let Some(id) = change.param_id()
            {
                self.set(id.get(), change.value());
            }
        }
    }

    /// Reports the gestures the test hook queued as the editor's output events.
    fn emit_edits(&self, output: &mut OutputEvents) {
        let edits = std::mem::take(&mut *EDITS.lock().unwrap_or_else(|e| e.into_inner()));
        for (phase, param, value) in edits {
            let id = ClapId::new(param);
            match phase {
                PHASE_BEGIN => {
                    let _ = output.try_push(&ParamGestureBeginEvent::new(0, id));
                }
                PHASE_CHANGE => {
                    self.set(param, value);
                    let _ = output.try_push(&ParamValueEvent::new(0, id, Pckn::match_all(), value));
                }
                _ => {
                    let _ = output.try_push(&ParamGestureEndEvent::new(0, id));
                }
            }
        }
    }
}

impl PluginShared<'_> for GainShared {}

pub struct GainMain<'a> {
    shared: &'a GainShared,
    host: HostSharedHandle<'a>,
    host_gui: Option<HostGui>,
    gui: RefCell<GuiState>,
}

/// The editor's state: its view while it is attached and the size it wants.
struct GuiState {
    view: Option<ChildView>,
    size: (u32, u32),
}

impl<'a> PluginMainThread<'a, GainShared> for GainMain<'a> {}

pub struct GainAudio<'a> {
    shared: &'a GainShared,
    delay: [Vec<f32>; 2],
    position: usize,
}

impl Plugin for Gain {
    type AudioProcessor<'a> = GainAudio<'a>;
    type Shared<'a> = GainShared;
    type MainThread<'a> = GainMain<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&GainShared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<PluginGui>()
            .register::<PluginLatency>();
    }
}

impl<'a> PluginAudioProcessor<'a, GainShared, GainMain<'a>> for GainAudio<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &GainMain<'a>,
        shared: &'a GainShared,
        _audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self {
            shared,
            delay: [vec![0.0; LATENCY], vec![0.0; LATENCY]],
            position: 0,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        self.shared.handle_events(events.input);
        self.shared.emit_edits(events.output);
        let sign = if self.shared.inverted.load(Ordering::Relaxed) == 1 {
            -1.0
        } else {
            1.0
        };
        let gain = (self.shared.gain() * sign) as f32;
        let Some(mut pair) = audio.port_pair(0) else {
            return Ok(ProcessStatus::Continue);
        };
        let mut channels = pair
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("32-bit audio only"))?;
        let mut frames = 0;
        for index in 0..channels.channel_pair_count() {
            let Some(ChannelPair::InputOutput(input, output)) = channels.channel_pair(index) else {
                continue;
            };
            let line = &mut self.delay[index.min(1)];
            let mut at = self.position;
            for (sample_in, sample_out) in input.iter().zip(output.iter_mut()) {
                *sample_out = line[at] * gain;
                line[at] = *sample_in;
                at = (at + 1) % LATENCY;
            }
            frames = input.len();
        }
        self.position = (self.position + frames) % LATENCY;
        Ok(ProcessStatus::Continue)
    }

    fn reset(&mut self) {
        for line in &mut self.delay {
            line.fill(0.0);
        }
        self.position = 0;
    }
}

impl PluginAudioProcessorParams for GainAudio<'_> {
    fn flush(&mut self, input_parameter_changes: &InputEvents, output: &mut OutputEvents) {
        self.shared.handle_events(input_parameter_changes);
        self.shared.emit_edits(output);
    }
}

impl PluginAudioPortsImpl for GainMain<'_> {
    fn count(&self, _is_input: bool) -> u32 {
        1
    }

    fn get(&self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            stereo_port(writer);
        }
    }
}

impl PluginLatencyImpl for GainMain<'_> {
    fn get(&self) -> u32 {
        crate::reported_latency()
    }
}

impl PluginMainThreadParams for GainMain<'_> {
    fn count(&self) -> u32 {
        2
    }

    fn get_info(&self, param_index: u32, info: &mut ParamInfoWriter) {
        match param_index {
            0 => info.set(&ParamInfo {
                id: ClapId::new(PARAM_GAIN),
                flags: ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Cookie::empty(),
                name: b"Gain",
                module: b"",
                min_value: 0.0,
                max_value: 2.0,
                default_value: 1.0,
            }),
            1 => info.set(&ParamInfo {
                id: ClapId::new(PARAM_POLARITY),
                flags: ParamInfoFlags::IS_AUTOMATABLE
                    | ParamInfoFlags::IS_STEPPED
                    | ParamInfoFlags::IS_ENUM,
                cookie: Cookie::empty(),
                name: b"Polarity",
                module: b"",
                min_value: 0.0,
                max_value: 1.0,
                default_value: 0.0,
            }),
            _ => {}
        }
    }

    fn get_value(&self, param_id: ClapId) -> Option<f64> {
        match param_id.get() {
            PARAM_GAIN => Some(self.shared.gain()),
            PARAM_POLARITY => Some(f64::from(self.shared.inverted.load(Ordering::Relaxed))),
            _ => None,
        }
    }

    fn value_to_text(
        &self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        match param_id.get() {
            PARAM_GAIN => write!(writer, "{value:.2}"),
            PARAM_POLARITY => writer.write_str(if value >= 0.5 { "Inverted" } else { "Normal" }),
            _ => Err(core::fmt::Error),
        }
    }

    fn text_to_value(&self, param_id: ClapId, text: &CStr) -> Option<f64> {
        let text = text.to_str().ok()?;
        match param_id.get() {
            PARAM_GAIN => text.trim().parse().ok(),
            PARAM_POLARITY => Some(f64::from(u8::from(text.eq_ignore_ascii_case("inverted")))),
            _ => None,
        }
    }

    fn flush(&self, input_parameter_changes: &InputEvents, _output: &mut OutputEvents) {
        self.shared.handle_events(input_parameter_changes);
    }
}

impl PluginStateImpl for GainMain<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        let mut bytes = Vec::with_capacity(STATE_LEN);
        bytes.extend_from_slice(STATE_MAGIC);
        bytes.extend_from_slice(&self.shared.gain().to_le_bytes());
        bytes.push(u8::from(self.shared.inverted.load(Ordering::Relaxed) == 1));
        output.write_all(&bytes)?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes)?;
        if bytes.len() != STATE_LEN || &bytes[..4] != STATE_MAGIC {
            return Err(PluginError::Message("not a fixture gain state"));
        }
        let gain = f64::from_le_bytes(bytes[4..12].try_into().unwrap_or([0; 8]));
        self.shared.set(PARAM_GAIN, gain);
        self.shared.set(PARAM_POLARITY, f64::from(bytes[12]));
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Gain editor (CLAP gui extension)

/// Gestures queued by `ow_fixture_simulate_edit`: (phase, parameter, plain value).
static EDITS: Mutex<Vec<(u32, u32, f64)>> = Mutex::new(Vec::new());
/// The instance whose editor is open (set by `gui.create`, cleared by `gui.destroy`).
static ACTIVE_GUI: AtomicPtr<GainMain<'static>> = AtomicPtr::new(std::ptr::null_mut());

/// The GUI API this platform's fixture editor embeds (none on Linux).
fn native_api() -> Option<GuiApiType<'static>> {
    if cfg!(windows) {
        Some(GuiApiType::WIN32)
    } else if cfg!(target_os = "macos") {
        Some(GuiApiType::COCOA)
    } else {
        None
    }
}

impl GainMain<'_> {
    fn resize_view(&self, width: u32, height: u32) {
        let mut gui = self.gui.borrow_mut();
        gui.size = (width, height);
        if let Some(view) = &gui.view {
            view.set_size(width, height);
        }
    }
}

impl PluginGuiImpl for GainMain<'_> {
    fn is_api_supported(&self, configuration: GuiConfiguration) -> bool {
        !configuration.is_floating && native_api().is_some_and(|api| api == configuration.api_type)
    }

    fn get_preferred_api(&self) -> Option<GuiConfiguration<'_>> {
        native_api().map(|api_type| GuiConfiguration {
            api_type,
            is_floating: false,
        })
    }

    fn create(&self, _configuration: GuiConfiguration) -> Result<(), PluginError> {
        ACTIVE_GUI.store(
            self as *const GainMain<'_> as *const GainMain<'static> as *mut GainMain<'static>,
            Ordering::SeqCst,
        );
        Ok(())
    }

    fn destroy(&self) {
        ACTIVE_GUI.store(std::ptr::null_mut(), Ordering::SeqCst);
        self.gui.borrow_mut().view = None;
    }

    fn set_scale(&self, _scale: f64) -> Result<(), PluginError> {
        Ok(())
    }

    fn get_size(&self) -> Option<GuiSize> {
        let (width, height) = self.gui.borrow().size;
        Some(GuiSize { width, height })
    }

    fn can_resize(&self) -> bool {
        true
    }

    fn adjust_size(&self, size: GuiSize) -> Option<GuiSize> {
        Some(GuiSize {
            width: size.width.max(100),
            height: size.height.max(80),
        })
    }

    fn set_size(&self, size: GuiSize) -> Result<(), PluginError> {
        self.resize_view(size.width, size.height);
        Ok(())
    }

    fn set_parent(&self, window: Window) -> Result<(), PluginError> {
        let parent = window
            .as_win32_hwnd()
            .or_else(|| window.as_cocoa_nsview())
            .ok_or(PluginError::Message("no native parent window"))?;
        let (width, height) = self.gui.borrow().size;
        let view = ChildView::create(parent, width, height)
            .ok_or(PluginError::Message("could not create the editor view"))?;
        self.gui.borrow_mut().view = Some(view);
        Ok(())
    }

    fn set_transient(&self, _window: Window) -> Result<(), PluginError> {
        Err(PluginError::Message("the editor is embedded, not floating"))
    }

    fn show(&self) -> Result<(), PluginError> {
        Ok(())
    }

    fn hide(&self) -> Result<(), PluginError> {
        Ok(())
    }
}

/// Test hook: the user's gesture on a parameter in the open editor. `phase`: 0 takes hold of
/// the control, 1 changes it to the plain `value`, 2 lets go. Delivered as output events by the
/// next `process` call.
#[unsafe(no_mangle)]
pub extern "C" fn ow_fixture_simulate_edit(param: u32, value: f64, phase: u32) {
    EDITS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((phase, param, value));
}

/// Test hook: the editor asks the host to resize its window. Returns 1 when the host accepted
/// (the view then follows). Call it on the plugin main thread.
#[unsafe(no_mangle)]
pub extern "C" fn ow_fixture_request_resize(width: u32, height: u32) -> u32 {
    let active = ACTIVE_GUI.load(Ordering::SeqCst);
    if active.is_null() {
        return 0;
    }
    // SAFETY: the pointer is set by `gui.create` and cleared by `gui.destroy`, both on the main
    // thread, which is also where this hook is called.
    let main = unsafe { &*active };
    let Some(host_gui) = main.host_gui else {
        return 0;
    };
    if host_gui.request_resize(&main.host, width, height).is_ok() {
        main.resize_view(width, height);
        1
    } else {
        0
    }
}

/// Test hook: whether the editor's view is attached and how big it is.
///
/// # Safety
/// `out` must be null or point at an `EditorState`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ow_fixture_editor_state(out: *mut EditorState) {
    if out.is_null() {
        return;
    }
    let active = ACTIVE_GUI.load(Ordering::SeqCst);
    let state = if active.is_null() {
        EditorState::default()
    } else {
        // SAFETY: as in `ow_fixture_request_resize`.
        let main = unsafe { &*active };
        let gui = main.gui.borrow();
        match &gui.view {
            Some(view) => {
                let (width, height) = view.size();
                EditorState {
                    attached: 1,
                    width,
                    height,
                }
            }
            None => EditorState::default(),
        }
    };
    // SAFETY: the caller guarantees `out`.
    unsafe { *out = state };
}

// ---------------------------------------------------------------------------------------------
// Tone

pub struct Tone;

#[derive(Default)]
pub struct ToneShared {
    level_bits: AtomicU64,
    initialised: AtomicU32,
}

impl ToneShared {
    fn level(&self) -> f64 {
        if self.initialised.load(Ordering::Relaxed) == 0 {
            0.5
        } else {
            f64::from_bits(self.level_bits.load(Ordering::Relaxed))
        }
    }

    fn handle_params(&self, events: &InputEvents) {
        for event in events {
            if let Some(CoreEventSpace::ParamValue(change)) = event.as_core_event()
                && change.param_id().map(ClapId::get) == Some(PARAM_LEVEL)
            {
                self.level_bits
                    .store(change.value().clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                self.initialised.store(1, Ordering::Relaxed);
            }
        }
    }
}

impl PluginShared<'_> for ToneShared {}

pub struct ToneMain<'a> {
    shared: &'a ToneShared,
}

impl<'a> PluginMainThread<'a, ToneShared> for ToneMain<'a> {}

pub struct ToneAudio<'a> {
    shared: &'a ToneShared,
    /// The velocity of the held note, 0 when none is held.
    held: f32,
}

/// Most note changes one block applies at their sample offset; more are ignored.
const MAX_CHANGES: usize = 64;

impl Plugin for Tone {
    type AudioProcessor<'a> = ToneAudio<'a>;
    type Shared<'a> = ToneShared;
    type MainThread<'a> = ToneMain<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&ToneShared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>();
    }
}

impl<'a> PluginAudioProcessor<'a, ToneShared, ToneMain<'a>> for ToneAudio<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &ToneMain<'a>,
        shared: &'a ToneShared,
        _audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self { shared, held: 0.0 })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        self.shared.handle_params(events.input);
        let level = self.shared.level() as f32;
        // Note on and off events as (frame, velocity) pairs, in order.
        let mut changes = [(0_usize, 0.0_f32); MAX_CHANGES];
        let mut count = 0;
        for event in events.input {
            let change = match event.as_core_event() {
                Some(CoreEventSpace::NoteOn(note)) => {
                    (note.header().time() as usize, note.velocity() as f32)
                }
                Some(CoreEventSpace::NoteOff(note)) => (note.header().time() as usize, 0.0),
                _ => continue,
            };
            if count < MAX_CHANGES {
                changes[count] = change;
                count += 1;
            }
        }
        let Some(mut pair) = audio.port_pair(0) else {
            return Ok(ProcessStatus::Continue);
        };
        let mut channels = pair
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("32-bit audio only"))?;
        let start = self.held;
        let mut end = start;
        for index in 0..channels.channel_pair_count() {
            let output = match channels.channel_pair(index) {
                Some(
                    ChannelPair::OutputOnly(output)
                    | ChannelPair::InputOutput(_, output)
                    | ChannelPair::InPlace(output),
                ) => output,
                _ => continue,
            };
            let mut held = start;
            let mut next = 0;
            for (frame, sample) in output.iter_mut().enumerate() {
                while next < count && changes[next].0 <= frame {
                    held = changes[next].1;
                    next += 1;
                }
                *sample = held * level;
            }
            end = held;
        }
        self.held = end;
        Ok(ProcessStatus::Continue)
    }

    fn reset(&mut self) {
        self.held = 0.0;
    }
}

impl PluginAudioProcessorParams for ToneAudio<'_> {
    fn flush(&mut self, input_parameter_changes: &InputEvents, _output: &mut OutputEvents) {
        self.shared.handle_params(input_parameter_changes);
    }
}

impl PluginAudioPortsImpl for ToneMain<'_> {
    fn count(&self, is_input: bool) -> u32 {
        u32::from(!is_input)
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 && !is_input {
            stereo_port(writer);
        }
    }
}

impl PluginNotePortsImpl for ToneMain<'_> {
    fn count(&self, is_input: bool) -> u32 {
        u32::from(is_input)
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if index == 0 && is_input {
            writer.set(&NotePortInfo {
                id: ClapId::new(0),
                name: b"notes",
                supported_dialects: NoteDialects::CLAP,
                preferred_dialect: Some(NoteDialect::Clap),
            });
        }
    }
}

impl PluginMainThreadParams for ToneMain<'_> {
    fn count(&self) -> u32 {
        1
    }

    fn get_info(&self, param_index: u32, info: &mut ParamInfoWriter) {
        if param_index == 0 {
            info.set(&ParamInfo {
                id: ClapId::new(PARAM_LEVEL),
                flags: ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Cookie::empty(),
                name: b"Level",
                module: b"",
                min_value: 0.0,
                max_value: 1.0,
                default_value: 0.5,
            });
        }
    }

    fn get_value(&self, param_id: ClapId) -> Option<f64> {
        (param_id.get() == PARAM_LEVEL).then(|| self.shared.level())
    }

    fn value_to_text(
        &self,
        _param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        write!(writer, "{value:.2}")
    }

    fn text_to_value(&self, _param_id: ClapId, text: &CStr) -> Option<f64> {
        text.to_str().ok()?.trim().parse().ok()
    }

    fn flush(&self, input_parameter_changes: &InputEvents, _output: &mut OutputEvents) {
        self.shared.handle_params(input_parameter_changes);
    }
}
