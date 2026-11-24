//! The audio-thread half of a VST3 instance.
//!
//! [`AudioSide`] holds the `IAudioProcessor`, the preallocated channel buffers and the event and
//! parameter lists handed to `process`. It sits behind the same kind of mutex as CLAP's: the audio
//! thread only `try_lock`s it, the control side takes it for a few instructions to deactivate or
//! re-activate the plugin.

#![allow(unsafe_code)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use ow_dsp::{Event, EventKind};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

use super::host::{Edit, EventList, ParamChanges};
use crate::hosted::PluginProcessor;

/// Distinct parameters one block can carry.
const PARAM_SLOTS: usize = 256;
/// Note events one block can carry.
const EVENT_SLOTS: usize = 1024;
/// Edits one block takes from the queue (the rest wait for the next one).
const MAX_EDITS_PER_BLOCK: usize = 512;

/// What activation needs and produces: the plugin's main buses as the host uses them.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Layout {
    /// Channels of the main audio input bus (0 when there is none).
    pub in_channels: usize,
    /// Channels of the main audio output bus (0 when there is none).
    pub out_channels: usize,
    /// How many audio input buses the plugin has (the main one is index 0).
    pub in_buses: usize,
    /// How many audio output buses the plugin has.
    pub out_buses: usize,
    /// True when the plugin has an event input bus (notes).
    pub note_input: bool,
}

pub(super) struct AudioSide {
    processor: ComPtr<IAudioProcessor>,
    layout: Layout,
    params_in: ComWrapper<ParamChanges>,
    params_in_ptr: ComPtr<IParameterChanges>,
    params_out: ComWrapper<ParamChanges>,
    params_out_ptr: ComPtr<IParameterChanges>,
    events_in: ComWrapper<EventList>,
    events_in_ptr: ComPtr<IEventList>,
    events_out: ComWrapper<EventList>,
    events_out_ptr: ComPtr<IEventList>,
    /// One buffer per channel of the main input bus, `max_block` frames each.
    in_buffers: Vec<Vec<f32>>,
    out_buffers: Vec<Vec<f32>>,
    /// Channel pointers into the buffers above (what `AudioBusBuffers` points at).
    in_pointers: Vec<*mut f32>,
    out_pointers: Vec<*mut f32>,
    in_buses: Vec<AudioBusBuffers>,
    out_buses: Vec<AudioBusBuffers>,
    context: ProcessContext,
    /// Frames processed since the audio side was created.
    steady: i64,
    /// `setProcessing(true)` has been called.
    processing: bool,
    failed: bool,
}

// SAFETY: the audio side is moved between threads only under its mutex, and the raw pointers
// point into buffers it owns (heap storage that does not move with the struct). The COM objects
// are used by one thread at a time.
unsafe impl Send for AudioSide {}

impl AudioSide {
    pub(super) fn new(
        processor: ComPtr<IAudioProcessor>,
        layout: Layout,
        max_block: usize,
        sample_rate: f64,
    ) -> Self {
        let params_in = ComWrapper::new(ParamChanges::new(PARAM_SLOTS));
        let params_out = ComWrapper::new(ParamChanges::new(16));
        let events_in = ComWrapper::new(EventList::new(EVENT_SLOTS));
        let events_out = ComWrapper::new(EventList::new(EVENT_SLOTS));
        let mut in_buffers = vec![vec![0.0_f32; max_block]; layout.in_channels];
        let mut out_buffers = vec![vec![0.0_f32; max_block]; layout.out_channels];
        let in_pointers: Vec<*mut f32> = in_buffers.iter_mut().map(|b| b.as_mut_ptr()).collect();
        let out_pointers: Vec<*mut f32> = out_buffers.iter_mut().map(|b| b.as_mut_ptr()).collect();
        // SAFETY: plain old data; zeroes are valid (no flags, no timing information).
        let mut context: ProcessContext = unsafe { std::mem::zeroed() };
        context.sampleRate = sample_rate;
        Self {
            processor,
            layout,
            params_in_ptr: params_in
                .to_com_ptr::<IParameterChanges>()
                .expect("IParameterChanges"),
            params_in,
            params_out_ptr: params_out
                .to_com_ptr::<IParameterChanges>()
                .expect("IParameterChanges"),
            params_out,
            events_in_ptr: events_in.to_com_ptr::<IEventList>().expect("IEventList"),
            events_in,
            events_out_ptr: events_out.to_com_ptr::<IEventList>().expect("IEventList"),
            events_out,
            in_buffers,
            out_buffers,
            in_pointers,
            out_pointers,
            in_buses: bus_buffers(layout.in_buses),
            out_buses: bus_buffers(layout.out_buses),
            context,
            steady: 0,
            processing: false,
            failed: false,
        }
    }

    /// Stops processing (`setProcessing(false)`) when it was started. Idempotent.
    pub(super) fn stop_processing(&mut self) {
        if self.processing {
            self.processing = false;
            // SAFETY: a live processor; `process` is not running (the caller holds the mutex).
            unsafe { self.processor.setProcessing(0) };
        }
    }

    /// Runs the plugin on `audio` with the queued edits and the note `events`. Returns false when
    /// the block was not processed.
    fn run(
        &mut self,
        edits: &mut rtrb::Consumer<Edit>,
        carry: &mut Option<Edit>,
        audio: &mut [&mut [f32]; 2],
        events: &[Event],
    ) -> bool {
        let frames = audio[0].len().min(audio[1].len());
        let max_block = self
            .out_buffers
            .first()
            .or(self.in_buffers.first())
            .map_or(frames, Vec::len);
        let frames = frames.min(max_block);
        if self.failed || frames == 0 {
            return false;
        }

        // Parameter edits first, notes after; a block that holds more than fits waits.
        self.params_in.clear();
        self.params_out.clear();
        self.events_in.clear();
        self.events_out.clear();
        fill_params(&self.params_in, carry, edits, MAX_EDITS_PER_BLOCK);
        for event in events.iter().take(EVENT_SLOTS) {
            let offset = i32::try_from(event.offset).unwrap_or(0);
            let offset = offset.min(i32::try_from(frames - 1).unwrap_or(0));
            self.events_in.push(note_event(event.kind, offset));
        }

        // Input: the main bus carries the signal (a mono plugin gets the sum).
        match self.layout.in_channels {
            0 => {}
            1 => {
                for (index, out) in self.in_buffers[0][..frames].iter_mut().enumerate() {
                    *out = 0.5 * (audio[0][index] + audio[1][index]);
                }
            }
            _ => {
                self.in_buffers[0][..frames].copy_from_slice(&audio[0][..frames]);
                self.in_buffers[1][..frames].copy_from_slice(&audio[1][..frames]);
            }
        }

        if !self.processing {
            // SAFETY: a live processor, called from the thread that processes.
            let started =
                assert_no_alloc::permit_alloc(|| unsafe { self.processor.setProcessing(1) });
            if started != kResultOk && started != kNotImplemented {
                self.failed = true;
                return false;
            }
            self.processing = true;
        }

        if let Some(bus) = self.in_buses.first_mut() {
            bus.numChannels = self.layout.in_channels as i32;
            bus.silenceFlags = 0;
            bus.__field0.channelBuffers32 = self.in_pointers.as_mut_ptr();
        }
        if let Some(bus) = self.out_buses.first_mut() {
            bus.numChannels = self.layout.out_channels as i32;
            bus.silenceFlags = 0;
            bus.__field0.channelBuffers32 = self.out_pointers.as_mut_ptr();
        }
        self.context.projectTimeSamples = self.steady;
        self.context.continousTimeSamples = self.steady;
        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: frames as i32,
            numInputs: self.in_buses.len() as i32,
            numOutputs: self.out_buses.len() as i32,
            inputs: if self.in_buses.is_empty() {
                std::ptr::null_mut()
            } else {
                self.in_buses.as_mut_ptr()
            },
            outputs: if self.out_buses.is_empty() {
                std::ptr::null_mut()
            } else {
                self.out_buses.as_mut_ptr()
            },
            inputParameterChanges: self.params_in_ptr.as_ptr(),
            outputParameterChanges: self.params_out_ptr.as_ptr(),
            inputEvents: self.events_in_ptr.as_ptr(),
            outputEvents: self.events_out_ptr.as_ptr(),
            processContext: &mut self.context,
        };
        // SAFETY: a live processor; `data` and everything it points at outlive the call. The
        // plugin's own allocations are its business (see the crate docs on realtime).
        let result = assert_no_alloc::permit_alloc(|| unsafe { self.processor.process(&mut data) });
        self.steady += frames as i64;
        if result != kResultOk && result != kResultTrue {
            self.failed = true;
            return false;
        }

        // Output: the main bus's first channels; a mono plugin plays on both sides.
        match self.layout.out_channels {
            0 => {
                audio[0][..frames].fill(0.0);
                audio[1][..frames].fill(0.0);
            }
            1 => {
                audio[0][..frames].copy_from_slice(&self.out_buffers[0][..frames]);
                audio[1][..frames].copy_from_slice(&self.out_buffers[0][..frames]);
            }
            _ => {
                audio[0][..frames].copy_from_slice(&self.out_buffers[0][..frames]);
                audio[1][..frames].copy_from_slice(&self.out_buffers[1][..frames]);
            }
        }
        true
    }
}

/// Moves queued edits into this block's parameter list. The latest value of a parameter within a
/// block wins. An edit that does not fit (more distinct parameters than the list holds) is kept
/// in `carry` for the next block instead of being dropped.
fn fill_params(
    params: &ParamChanges,
    carry: &mut Option<Edit>,
    edits: &mut rtrb::Consumer<Edit>,
    limit: usize,
) -> usize {
    let mut taken = 0;
    while taken < limit {
        let edit = match carry.take() {
            Some(edit) => edit,
            None => match edits.pop() {
                Ok(edit) => edit,
                Err(_) => break,
            },
        };
        if !params.set_last(edit.id, 0, edit.value) {
            *carry = Some(edit);
            break;
        }
        taken += 1;
    }
    taken
}

/// `AudioBusBuffers` for `count` buses, all empty: the main bus is filled in per block, the
/// others stay inactive.
fn bus_buffers(count: usize) -> Vec<AudioBusBuffers> {
    (0..count)
        .map(|_| AudioBusBuffers {
            numChannels: 0,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: std::ptr::null_mut(),
            },
        })
        .collect()
}

/// A note event of the engine as a VST3 event.
fn note_event(kind: EventKind, offset: i32) -> VstEvent {
    // SAFETY: `Event` is plain old data: all zeroes is a valid (empty) event.
    let mut event: VstEvent = unsafe { std::mem::zeroed() };
    event.busIndex = 0;
    event.sampleOffset = offset;
    match kind {
        EventKind::NoteOn { key, velocity } => {
            event.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
            event.__field0.noteOn = NoteOnEvent {
                channel: 0,
                pitch: i16::from(key),
                tuning: 0.0,
                velocity: f32::from(velocity) / 127.0,
                length: 0,
                noteId: i32::from(key),
            };
        }
        EventKind::NoteOff { key } => {
            event.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
            event.__field0.noteOff = NoteOffEvent {
                channel: 0,
                pitch: i16::from(key),
                velocity: 0.0,
                noteId: i32::from(key),
                tuning: 0.0,
            };
        }
    }
    event
}

/// The SDK's event struct (`Event` alone is the engine's note event here).
type VstEvent = vst3::Steinberg::Vst::Event;

/// The audio side and the edits that feed it. Locked briefly by the control side; only
/// `try_lock`ed by the audio thread.
pub(super) struct AudioSlot {
    pub(super) side: Option<AudioSide>,
    pub(super) edits: rtrb::Consumer<Edit>,
    /// An edit the last block had no room for.
    pub(super) carry: Option<Edit>,
}

pub(super) type SharedSlot = Arc<Mutex<AudioSlot>>;

/// The processor a device node owns.
pub(super) struct Vst3Processor {
    pub(super) slot: SharedSlot,
    pub(super) latency: Arc<AtomicU32>,
    /// True for a plugin that makes sound from notes: it is silent when a block is skipped.
    pub(super) instrument: bool,
}

impl PluginProcessor for Vst3Processor {
    fn process(&mut self, audio: &mut [&mut [f32]; 2], events: &[Event]) {
        // A held lock means the control side is tearing the instance down: skip the block
        // (an effect passes its input through, an instrument is silent).
        let processed = match self.slot.try_lock() {
            Ok(mut slot) => run_slot(&mut slot, audio, events),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                run_slot(&mut poisoned.into_inner(), audio, events)
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
            // Stopping processing resets the plugin; the next block starts it again.
            assert_no_alloc::permit_alloc(|| side.stop_processing());
        }
    }

    fn latency_frames(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }
}

fn run_slot(slot: &mut AudioSlot, audio: &mut [&mut [f32]; 2], events: &[Event]) -> bool {
    let AudioSlot { side, edits, carry } = slot;
    match side.as_mut() {
        Some(side) => side.run(edits, carry, audio, events),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_beyond_the_blocks_parameter_slots_wait_for_the_next_block() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::new(64);
        for id in 0..10_u32 {
            producer
                .push(Edit {
                    id,
                    value: f64::from(id) / 10.0,
                })
                .unwrap();
        }
        // A block with room for four distinct parameters.
        let list = ComWrapper::new(ParamChanges::new(4));
        let pointer = list.to_com_ptr::<IParameterChanges>().unwrap();
        let mut carry = None;
        let mut seen = Vec::new();
        for _ in 0..4 {
            list.clear();
            fill_params(&list, &mut carry, &mut consumer, 512);
            // SAFETY: the list is alive; the queues it hands out live as long as it does.
            unsafe {
                for index in 0..pointer.getParameterCount() {
                    let queue = vst3::ComRef::from_raw(pointer.getParameterData(index)).unwrap();
                    let (mut offset, mut value) = (0, 0.0);
                    queue.getPoint(0, &mut offset, &mut value);
                    seen.push((queue.getParameterId(), value));
                }
            }
        }
        assert_eq!(seen.len(), 10, "every edit reaches a block: {seen:?}");
        for (id, value) in seen {
            assert_eq!(value, f64::from(id) / 10.0);
        }
        assert!(carry.is_none());
    }
}
