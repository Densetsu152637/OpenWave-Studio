//! MIDI ports: the [`MidiBackend`] abstraction and the `midir` implementation.
//!
//! Every input connection runs its handler on a thread owned by the platform MIDI layer, so each
//! connection is its own producer: the application gives it its own wait-free ring to the audio
//! thread (see `ow-engine`'s `CoreBuilder::producer_ring`) and the handler pushes into that ring
//! only. [`MidiEvent`] is a small `Copy` value that stays valid on that path: it carries a
//! monotonic microsecond timestamp taken when the event reached the handler.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::backend::{IoError, IoErrorKind};

/// A short channel or system message (status plus up to two data bytes) with a timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiEvent {
    /// Microseconds on the [`now_micros`] clock when the event was received (or, for the mock
    /// backend, the timestamp the test supplied).
    pub timestamp_us: u64,
    /// Index of the input connection that produced it (the `source` given at connect time).
    pub source: u16,
    /// Number of valid bytes in `bytes` (1 to 3).
    pub len: u8,
    pub bytes: [u8; 3],
}

impl MidiEvent {
    /// Builds an event from raw bytes; `None` for empty or longer messages (SysEx is not
    /// carried on the realtime path).
    pub fn from_bytes(source: u16, timestamp_us: u64, raw: &[u8]) -> Option<Self> {
        if raw.is_empty() || raw.len() > 3 {
            return None;
        }
        let mut bytes = [0; 3];
        bytes[..raw.len()].copy_from_slice(raw);
        Some(Self {
            timestamp_us,
            source,
            len: raw.len() as u8,
            bytes,
        })
    }

    pub fn status(&self) -> u8 {
        self.bytes[0]
    }

    pub fn data1(&self) -> u8 {
        self.bytes[1]
    }

    pub fn data2(&self) -> u8 {
        self.bytes[2]
    }
}

/// Microseconds on a process-wide monotonic clock (zero at first use).
pub fn now_micros() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// A MIDI port as shown in the preferences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiPortInfo {
    /// Stable identifier to store in preferences (platform specific).
    pub id: String,
    pub name: String,
}

/// Handler run on the connection's own thread for every received message.
pub type MidiHandler = Box<dyn FnMut(MidiEvent) + Send>;

/// An open MIDI input. Dropping it disconnects the port and stops the handler.
pub trait MidiInputConnection: Send {}

/// An open MIDI output.
pub trait MidiOutputConnection: Send {
    fn send(&mut self, message: &[u8]) -> Result<(), IoError>;
}

/// A source of MIDI ports.
pub trait MidiBackend: Send + Sync {
    fn inputs(&self) -> Result<Vec<MidiPortInfo>, IoError>;

    fn outputs(&self) -> Result<Vec<MidiPortInfo>, IoError>;

    /// Connects to an input port by stored id (or, failing that, by name). `source` is stamped
    /// on every event so a shared consumer can tell inputs apart.
    fn connect_input(
        &self,
        port_id: &str,
        source: u16,
        handler: MidiHandler,
    ) -> Result<Box<dyn MidiInputConnection>, IoError>;

    fn connect_output(&self, port_id: &str) -> Result<Box<dyn MidiOutputConnection>, IoError>;
}

/// Platform MIDI through `midir` (CoreMIDI, WinRT/WinMM, ALSA sequencer).
#[derive(Debug, Default)]
pub struct MidirBackend {
    dropped_long: Arc<AtomicU64>,
}

impl MidirBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Messages longer than three bytes (SysEx) that were discarded across all inputs.
    pub fn dropped_long_messages(&self) -> u64 {
        self.dropped_long.load(Ordering::Relaxed)
    }
}

const CLIENT_NAME: &str = "OpenWave Studio";

fn midir_error(context: &str, cause: impl std::fmt::Display) -> IoError {
    IoError::new(context, cause)
}

/// A midir input owned by its own thread: some platform connection types are not `Send`
/// (ALSA, CoreMIDI), so the connection is created, kept and dropped on one thread and this
/// handle only holds the stop channel.
struct MidirInput {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl MidiInputConnection for MidirInput {}

impl Drop for MidirInput {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

enum OutputCommand {
    Send(Vec<u8>),
}

/// A midir output owned by its own thread, like [`MidirInput`].
struct MidirOutput {
    commands: Option<mpsc::Sender<OutputCommand>>,
    thread: Option<JoinHandle<()>>,
}

impl MidiOutputConnection for MidirOutput {
    fn send(&mut self, message: &[u8]) -> Result<(), IoError> {
        self.commands
            .as_ref()
            .and_then(|tx| tx.send(OutputCommand::Send(message.to_vec())).ok())
            .ok_or_else(|| IoError::new("sending MIDI", "the output thread ended"))
    }
}

impl Drop for MidirOutput {
    fn drop(&mut self) {
        self.commands.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn open_input(
    port_id: &str,
    source: u16,
    mut handler: MidiHandler,
    dropped: Arc<AtomicU64>,
) -> Result<midir::MidiInputConnection<()>, IoError> {
    let mut input =
        midir::MidiInput::new(CLIENT_NAME).map_err(|e| midir_error("opening MIDI", e))?;
    // Notes, controllers and clock-free channel messages only: no SysEx, timing, sensing.
    input.ignore(midir::Ignore::All);
    let port = input
        .find_port_by_id(port_id)
        .or_else(|| {
            input
                .ports()
                .into_iter()
                .find(|port| input.port_name(port).is_ok_and(|name| name == port_id))
        })
        .ok_or_else(|| {
            IoError::with_kind(
                IoErrorKind::NotFound,
                "connecting MIDI input",
                format_args!("port \"{port_id}\" not found"),
            )
        })?;
    let name = input
        .port_name(&port)
        .unwrap_or_else(|_| port_id.to_string());
    input
        .connect(
            &port,
            &format!("{CLIENT_NAME} input"),
            move |_, raw, _| match MidiEvent::from_bytes(source, now_micros(), raw) {
                Some(event) => handler(event),
                None => {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            },
            (),
        )
        .map_err(|e| midir_error(&format!("connecting to \"{name}\""), e))
}

fn open_output(port_id: &str) -> Result<midir::MidiOutputConnection, IoError> {
    let output = midir::MidiOutput::new(CLIENT_NAME).map_err(|e| midir_error("opening MIDI", e))?;
    let port = output
        .find_port_by_id(port_id)
        .or_else(|| {
            output
                .ports()
                .into_iter()
                .find(|port| output.port_name(port).is_ok_and(|name| name == port_id))
        })
        .ok_or_else(|| {
            IoError::with_kind(
                IoErrorKind::NotFound,
                "connecting MIDI output",
                format_args!("port \"{port_id}\" not found"),
            )
        })?;
    output
        .connect(&port, &format!("{CLIENT_NAME} output"))
        .map_err(|e| midir_error("connecting MIDI output", e))
}

impl MidiBackend for MidirBackend {
    fn inputs(&self) -> Result<Vec<MidiPortInfo>, IoError> {
        let input =
            midir::MidiInput::new(CLIENT_NAME).map_err(|e| midir_error("opening MIDI", e))?;
        Ok(input
            .ports()
            .iter()
            .map(|port| MidiPortInfo {
                id: port.id(),
                name: input.port_name(port).unwrap_or_else(|_| port.id()),
            })
            .collect())
    }

    fn outputs(&self) -> Result<Vec<MidiPortInfo>, IoError> {
        let output =
            midir::MidiOutput::new(CLIENT_NAME).map_err(|e| midir_error("opening MIDI", e))?;
        Ok(output
            .ports()
            .iter()
            .map(|port| MidiPortInfo {
                id: port.id(),
                name: output.port_name(port).unwrap_or_else(|_| port.id()),
            })
            .collect())
    }

    fn connect_input(
        &self,
        port_id: &str,
        source: u16,
        handler: MidiHandler,
    ) -> Result<Box<dyn MidiInputConnection>, IoError> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), IoError>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let port = port_id.to_string();
        let dropped = Arc::clone(&self.dropped_long);
        let thread = thread::Builder::new()
            .name("ow-midi-input".into())
            .spawn(move || match open_input(&port, source, handler, dropped) {
                Ok(connection) => {
                    let _ = ready_tx.send(Ok(()));
                    // Held until the handle is dropped.
                    let _ = stop_rx.recv();
                    drop(connection);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|e| midir_error("spawning the MIDI input thread", e))?;
        ready_rx
            .recv()
            .map_err(|_| midir_error("connecting MIDI input", "the input thread ended"))??;
        Ok(Box::new(MidirInput {
            stop: Some(stop_tx),
            thread: Some(thread),
        }))
    }

    fn connect_output(&self, port_id: &str) -> Result<Box<dyn MidiOutputConnection>, IoError> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), IoError>>();
        let (command_tx, command_rx) = mpsc::channel::<OutputCommand>();
        let port = port_id.to_string();
        let thread = thread::Builder::new()
            .name("ow-midi-output".into())
            .spawn(move || match open_output(&port) {
                Ok(mut connection) => {
                    let _ = ready_tx.send(Ok(()));
                    while let Ok(OutputCommand::Send(bytes)) = command_rx.recv() {
                        let _ = connection.send(&bytes);
                    }
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|e| midir_error("spawning the MIDI output thread", e))?;
        ready_rx
            .recv()
            .map_err(|_| midir_error("connecting MIDI output", "the output thread ended"))??;
        Ok(Box::new(MidirOutput {
            commands: Some(command_tx),
            thread: Some(thread),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_carry_up_to_three_bytes() {
        let event = MidiEvent::from_bytes(2, 99, &[0x90, 60, 100]).unwrap();
        assert_eq!(
            (event.status(), event.data1(), event.data2(), event.len),
            (0x90, 60, 100, 3)
        );
        let short = MidiEvent::from_bytes(0, 0, &[0xC0, 5]).unwrap();
        assert_eq!((short.len, short.data2()), (2, 0));
        assert!(MidiEvent::from_bytes(0, 0, &[]).is_none());
        assert!(MidiEvent::from_bytes(0, 0, &[0xF0, 1, 2, 0xF7]).is_none());
    }

    #[test]
    fn clock_is_monotonic() {
        let a = now_micros();
        let b = now_micros();
        assert!(b >= a);
    }
}
