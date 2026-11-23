//! The gain effect and the tone instrument.
//!
//! Gain: stereo in, stereo out; `Gain` (id 1, 0 to 2) and `Polarity` (id 2, an enumeration);
//! the output is delayed by [`crate::GAIN_LATENCY`] frames and the plugin reports that latency.
//! State is a small binary blob holding both parameters. Tone: no audio input, one note input
//! port; while a note is held it outputs a constant level `Level * velocity`, which makes the
//! host's note events easy to assert on.

use std::ffi::CStr;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
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
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::plugin::features;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;

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
            Some(PluginInstance::new::<Gain>(
                host_info,
                &self.gain,
                |_| Ok(GainShared::default()),
                |_, shared| Ok(GainMain { shared }),
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
}

impl PluginShared<'_> for GainShared {}

pub struct GainMain<'a> {
    shared: &'a GainShared,
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
    fn flush(&mut self, input_parameter_changes: &InputEvents, _output: &mut OutputEvents) {
        self.shared.handle_events(input_parameter_changes);
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
        GAIN_LATENCY
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
