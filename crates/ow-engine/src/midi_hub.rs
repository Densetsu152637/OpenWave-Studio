//! MIDI input routing into the audio thread.
//!
//! Each connected MIDI input runs its handler on a thread owned by the platform MIDI layer, so
//! each one needs its own single-producer ring (`rtrb` rings are SPSC). The engine creates a
//! fixed pool of rings with the audio core ([`MIDI_RINGS`]) plus one ring for control surfaces;
//! connecting an input claims a free ring, disconnecting returns it. Inputs flagged as control
//! surfaces share the dedicated control-surface ring behind a mutex that only MIDI threads take;
//! the audio thread never locks anything.
//!
//! A control surface's messages are addressed to one target node. Every other input sends
//! [`Message::MidiIn`](crate::graph::Message): the audio core places it on the block by its
//! timestamp and hands it to the armed tracks' instruments and to the MIDI recorder. Overflow
//! drops the newest message and is counted.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use ow_io::{MidiBackend, MidiEvent, MidiInputConnection};

use crate::graph::{Message, MidiMessage, NodeId};
use crate::rings::RingSender;

/// Rings available for individual MIDI inputs per audio stream.
pub const MIDI_RINGS: usize = 8;
/// Capacity of each MIDI ring in messages.
pub const MIDI_RING_CAPACITY: usize = 1024;

/// One MIDI input the user enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiInputSelection {
    /// Port id (or name) from the MIDI backend.
    pub port: String,
    /// Route into the shared control-surface ring instead of a private ring.
    pub control_surface: bool,
}

/// Result of connecting one selected input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiPortState {
    pub port: String,
    pub connected: bool,
    pub error: Option<String>,
}

/// Snapshot for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiStatus {
    pub ports: Vec<MidiPortState>,
    /// Messages received from every input since start.
    pub events: u64,
    /// Messages dropped because a ring was full or no ring was free.
    pub dropped: u64,
    /// Last message as `[status, data1, data2]`, if any.
    pub last: Option<[u8; 3]>,
}

/// A ring that one connection owns.
struct Slot {
    sender: Mutex<RingSender<Message>>,
    in_use: AtomicBool,
}

/// What the audio core gives the hub when a stream (re)opens.
pub(crate) struct MidiRings {
    pub(crate) pool: Vec<RingSender<Message>>,
    pub(crate) control_surface: RingSender<Message>,
    pub(crate) target: NodeId,
}

#[derive(Default)]
struct Counters {
    events: AtomicU64,
    dropped: AtomicU64,
    /// `0x01000000 | status << 16 | data1 << 8 | data2`, or 0 when nothing arrived yet.
    last: AtomicU32,
}

struct State {
    selection: Vec<MidiInputSelection>,
    pool: Vec<Arc<Slot>>,
    control_surface: Option<Arc<Slot>>,
    target: NodeId,
    // Dropping a connection disconnects the port and ends its handler thread.
    active: Vec<Box<dyn MidiInputConnection>>,
    ports: Vec<MidiPortState>,
}

/// Connects MIDI inputs to the current audio core's rings.
pub struct MidiHub {
    backend: Arc<dyn MidiBackend>,
    state: Mutex<State>,
    counters: Arc<Counters>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl MidiHub {
    pub fn new(backend: Arc<dyn MidiBackend>) -> Self {
        Self {
            backend,
            state: Mutex::new(State {
                selection: Vec::new(),
                pool: Vec::new(),
                control_surface: None,
                target: NodeId(0),
                active: Vec::new(),
                ports: Vec::new(),
            }),
            counters: Arc::new(Counters::default()),
        }
    }

    pub fn backend(&self) -> &Arc<dyn MidiBackend> {
        &self.backend
    }

    /// Replaces the set of enabled inputs and connects them.
    pub fn set_inputs(&self, selection: Vec<MidiInputSelection>) -> Vec<MidiPortState> {
        let mut state = lock(&self.state);
        state.selection = selection;
        self.reconnect(&mut state);
        state.ports.clone()
    }

    pub fn status(&self) -> MidiStatus {
        let last = self.counters.last.load(Ordering::Relaxed);
        MidiStatus {
            ports: lock(&self.state).ports.clone(),
            events: self.counters.events.load(Ordering::Relaxed),
            dropped: self.counters.dropped.load(Ordering::Relaxed),
            last: (last != 0).then_some([(last >> 16) as u8, (last >> 8) as u8, last as u8]),
        }
    }

    /// Called when a new audio core exists: its rings replace the old pool and every selected
    /// input is reconnected to them.
    pub(crate) fn attach(&self, rings: MidiRings) {
        let mut state = lock(&self.state);
        // Old connections hold senders of the previous core's rings.
        state.active.clear();
        state.pool = rings
            .pool
            .into_iter()
            .map(|sender| {
                Arc::new(Slot {
                    sender: Mutex::new(sender),
                    in_use: AtomicBool::new(false),
                })
            })
            .collect();
        state.control_surface = Some(Arc::new(Slot {
            sender: Mutex::new(rings.control_surface),
            in_use: AtomicBool::new(false),
        }));
        state.target = rings.target;
        self.reconnect(&mut state);
    }

    /// Called when the audio stream closes: disconnects every input.
    pub(crate) fn detach(&self) {
        let mut state = lock(&self.state);
        state.active.clear();
        state.pool.clear();
        state.control_surface = None;
        for port in &mut state.ports {
            port.connected = false;
        }
    }

    fn reconnect(&self, state: &mut State) {
        state.active.clear();
        for slot in &state.pool {
            slot.in_use.store(false, Ordering::Relaxed);
        }
        let mut ports = Vec::with_capacity(state.selection.len());
        let selection = state.selection.clone();
        for (index, wanted) in selection.into_iter().enumerate() {
            let slot = if wanted.control_surface {
                state.control_surface.clone()
            } else {
                state
                    .pool
                    .iter()
                    .find(|slot| !slot.in_use.swap(true, Ordering::AcqRel))
                    .cloned()
            };
            let outcome = match slot {
                None if state.control_surface.is_none() => Err("audio is not running".to_string()),
                None => Err(format!("at most {MIDI_RINGS} MIDI inputs can be connected")),
                Some(slot) => self
                    .connect(&wanted, index as u16, &slot, state.target)
                    .map(|connection| state.active.push(connection))
                    .map_err(|error| error.to_string()),
            };
            ports.push(MidiPortState {
                port: wanted.port,
                connected: outcome.is_ok(),
                error: outcome.err(),
            });
        }
        state.ports = ports;
    }

    fn connect(
        &self,
        wanted: &MidiInputSelection,
        source: u16,
        slot: &Arc<Slot>,
        target: NodeId,
    ) -> Result<Box<dyn MidiInputConnection>, ow_io::IoError> {
        let counters = Arc::clone(&self.counters);
        let slot = Arc::clone(slot);
        let control_surface = wanted.control_surface;
        let handler = move |event: MidiEvent| {
            counters.events.fetch_add(1, Ordering::Relaxed);
            counters.last.store(
                0x0100_0000
                    | (u32::from(event.status()) << 16)
                    | (u32::from(event.data1()) << 8)
                    | u32::from(event.data2()),
                Ordering::Relaxed,
            );
            let message = MidiMessage {
                status: event.status(),
                data1: event.data1(),
                data2: event.data2(),
            };
            let pushed = lock(&slot.sender).push(if control_surface {
                // A control surface's messages go to one node and apply at the next cycle.
                Message::Midi {
                    node: target,
                    message,
                    offset: 0,
                }
            } else {
                // Notes and controllers for the armed tracks and the recorder, placed on the
                // block by the time they were received.
                Message::MidiIn {
                    message,
                    timestamp_us: event.timestamp_us,
                }
            });
            if !pushed {
                counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
        };
        self.backend
            .connect_input(&wanted.port, source, Box::new(handler))
    }
}

impl std::fmt::Debug for MidiHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiHub").finish_non_exhaustive()
    }
}
