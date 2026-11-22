//! Tauri state and commands wrapping the `ow-engine` handle.

use ow_engine::Engine;
use serde::Serialize;
use tauri::State;

/// Managed state: the engine, or why it could not start.
pub struct EngineState {
    result: Result<Engine, String>,
}

/// What the UI needs to render the transport and status bar. Mirrored in `src/lib/ipc.ts`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub running: bool,
    pub device_name: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub test_tone: bool,
    pub cpu_load: f32,
    pub error: Option<String>,
}

impl EngineState {
    /// Starts the engine and logs the opened device (or the failure) to stderr.
    pub fn start() -> Self {
        let result = Engine::start().map_err(|error| error.to_string());
        match &result {
            Ok(engine) => {
                let out = engine.output();
                eprintln!(
                    "[openwave] audio output: {} ({} Hz, {} ch)",
                    out.device_name, out.sample_rate, out.channels
                );
            }
            Err(error) => eprintln!("[openwave] audio output unavailable: {error}"),
        }
        Self { result }
    }

    pub fn status(&self) -> EngineStatus {
        match &self.result {
            Ok(engine) => {
                let out = engine.output();
                EngineStatus {
                    running: true,
                    device_name: Some(out.device_name.clone()),
                    sample_rate: Some(out.sample_rate),
                    channels: Some(out.channels),
                    test_tone: engine.test_tone(),
                    cpu_load: engine.cpu_load(),
                    error: None,
                }
            }
            Err(error) => EngineStatus {
                running: false,
                device_name: None,
                sample_rate: None,
                channels: None,
                test_tone: false,
                cpu_load: 0.0,
                error: Some(error.clone()),
            },
        }
    }

    fn set_test_tone(&self, on: bool) {
        if let Ok(engine) = &self.result {
            engine.set_test_tone(on);
        }
    }
}

#[tauri::command]
pub fn engine_status(state: State<'_, EngineState>) -> EngineStatus {
    state.status()
}

#[tauri::command]
pub fn set_test_tone(state: State<'_, EngineState>, on: bool) -> EngineStatus {
    state.set_test_tone(on);
    state.status()
}
