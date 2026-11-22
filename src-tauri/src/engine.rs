//! Tauri state and commands wrapping the `ow-engine` handle, device lists and preferences.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::thread;

use ow_engine::{Engine, EngineConfig, MidiInputSelection};
use ow_io::{AudioSelection, AudioState, DEFAULT_DEVICE, DeviceInfo, StreamRequest};
use serde::Serialize;
use tauri::State;

use crate::paths::Paths;
use crate::prefs::{self, Loaded, Preferences};

/// Managed state: the engine (which recovers on its own when the device fails), the stored
/// preferences and where they live.
pub struct EngineState {
    engine: Engine,
    prefs: Mutex<Preferences>,
    config_dir: PathBuf,
    /// Why defaults are in use, when the preferences file could not be loaded.
    prefs_warning: Mutex<Option<String>>,
}

/// MIDI side of the status, mirrored in `src/lib/ipc.ts`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiPortStatus {
    pub port: String,
    pub connected: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiStatusDto {
    pub ports: Vec<MidiPortStatus>,
    pub events: u64,
    pub dropped: u64,
    pub last: Option<[u8; 3]>,
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
    /// `running`, `recovering` or `failed`.
    pub audio_state: &'static str,
    pub recovery_attempt: u32,
    pub recovery_max: u32,
    pub recoveries: u32,
    pub backend: Option<String>,
    pub buffer_frames: Option<u32>,
    pub exclusive: bool,
    pub on_fallback_device: bool,
    pub midi: MidiStatusDto,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The audio selection stored in preferences, as the engine wants it.
pub fn audio_selection(prefs: &Preferences) -> AudioSelection {
    let audio = &prefs.audio;
    AudioSelection {
        backend: audio.backend.clone(),
        request: StreamRequest {
            output_device: audio.output_device.clone(),
            input_device: audio.input_device.clone(),
            sample_rate: audio.sample_rate,
            buffer_frames: audio.buffer_frames,
            output_channels: None,
        },
    }
}

fn midi_selection(prefs: &Preferences) -> Vec<MidiInputSelection> {
    prefs
        .midi
        .inputs
        .iter()
        .filter(|input| input.enabled)
        .map(|input| MidiInputSelection {
            port: input.port.clone(),
            control_surface: input.control_surface,
        })
        .collect()
}

impl EngineState {
    /// Loads preferences, starts the engine on the chosen device and logs the result to stderr.
    /// A device that cannot be opened is not fatal: the engine keeps retrying and the status
    /// carries the error.
    pub fn start(paths: &Paths) -> Self {
        let Loaded { prefs, warning } = prefs::load(&paths.config_dir);
        if let Some(warning) = &warning {
            eprintln!("[openwave] {warning}");
        }
        let engine = Engine::start_with(EngineConfig {
            audio: audio_selection(&prefs),
            midi_inputs: midi_selection(&prefs),
            ..EngineConfig::default()
        });
        let status = engine.audio_status();
        match &status.info {
            Some(info) => eprintln!(
                "[openwave] audio output: {} via {} ({} Hz, {} ch, buffer {})",
                info.device_name,
                info.backend,
                info.sample_rate,
                info.output_channels,
                info.buffer_frames
                    .map_or_else(|| "default".to_string(), |f| format!("{f} frames")),
            ),
            None => eprintln!(
                "[openwave] audio output unavailable: {}",
                status.error.as_deref().unwrap_or("unknown error")
            ),
        }
        Self::from_parts(engine, paths.config_dir.clone(), prefs, warning)
    }

    fn from_parts(
        engine: Engine,
        config_dir: PathBuf,
        prefs: Preferences,
        warning: Option<String>,
    ) -> Self {
        Self {
            engine,
            prefs: Mutex::new(prefs),
            config_dir,
            prefs_warning: Mutex::new(warning),
        }
    }

    pub fn status(&self) -> EngineStatus {
        let audio = self.engine.audio_status();
        let midi = self.engine.midi().status();
        let (audio_state, attempt, max) = match audio.state {
            AudioState::Running => ("running", 0, 0),
            AudioState::Recovering { attempt, max } => ("recovering", attempt, max),
            AudioState::Failed => ("failed", 0, 0),
        };
        let info = audio.info.as_ref();
        let error = match (&audio.state, &audio.error) {
            (AudioState::Recovering { attempt, max }, reason) => Some(format!(
                "Reconnecting the audio device (attempt {} of {max}){}",
                (*attempt).max(1),
                reason.as_ref().map_or(String::new(), |r| format!(": {r}"))
            )),
            (_, error) => error.clone(),
        };
        EngineStatus {
            running: info.is_some(),
            device_name: info.map(|i| i.device_name.clone()),
            sample_rate: info.map(|i| i.sample_rate),
            channels: info.map(|i| i.output_channels),
            test_tone: self.engine.test_tone(),
            cpu_load: self.engine.cpu_load(),
            error,
            audio_state,
            recovery_attempt: attempt,
            recovery_max: max,
            recoveries: audio.recoveries,
            backend: info.map(|i| i.backend.clone()),
            buffer_frames: info.and_then(|i| i.buffer_frames),
            exclusive: info.is_some_and(|i| i.exclusive),
            on_fallback_device: audio.on_fallback_device,
            midi: MidiStatusDto {
                ports: midi
                    .ports
                    .into_iter()
                    .map(|port| MidiPortStatus {
                        port: port.port,
                        connected: port.connected,
                        error: port.error,
                    })
                    .collect(),
                events: midi.events,
                dropped: midi.dropped,
                last: midi.last,
            },
        }
    }

    fn set_test_tone(&self, on: bool) {
        self.engine.set_test_tone(on);
    }

    /// Validates and stores `next`, applying audio and MIDI changes to the running engine. The
    /// new preferences are saved before the engine changes, so a failed save leaves everything
    /// as it was. An audio selection that cannot be opened is not kept (the engine keeps the
    /// previous one, the stored audio choice is put back) and is reported in `audio_error`;
    /// everything else is stored. The saved file, the in-memory preferences and the engine
    /// always agree; if they cannot be made to, the error says which part took effect.
    pub fn apply(&self, mut next: Preferences) -> Result<PrefsApplied, String> {
        next.version = prefs::PREFS_VERSION;
        next.validate()?;
        let mut current = lock(&self.prefs);
        prefs::save(&self.config_dir, &next)?;
        let mut audio_error = None;
        if next.audio != current.audio
            && let Err(error) = self.engine.reconfigure_audio(audio_selection(&next))
        {
            let reason = error.to_string();
            next.audio = current.audio.clone();
            if let Err(save_error) = prefs::save(&self.config_dir, &next) {
                // Put the previous file back so the stored audio choice matches the engine.
                return Err(match prefs::save(&self.config_dir, &current) {
                    Ok(()) => format!(
                        "The audio device could not be opened ({reason}) and the preferences could not be saved ({save_error}); nothing was changed."
                    ),
                    Err(restore_error) => format!(
                        "The audio device could not be opened ({reason}) and the preferences could not be saved ({save_error}; restoring the previous file also failed: {restore_error}). The engine keeps the previous audio device, but the stored preferences request the new one."
                    ),
                });
            }
            audio_error = Some(reason);
        }
        if next.midi != current.midi {
            self.engine.midi().set_inputs(midi_selection(&next));
        }
        *current = next.clone();
        *lock(&self.prefs_warning) = None;
        drop(current);
        Ok(PrefsApplied {
            prefs: next,
            audio_error,
            status: self.status(),
        })
    }
}

/// Result of `prefs_set`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefsApplied {
    pub prefs: Preferences,
    /// Why the requested audio device was not applied, when it was not.
    pub audio_error: Option<String>,
    pub status: EngineStatus,
}

/// Result of `prefs_get`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefsSnapshot {
    pub prefs: Preferences,
    pub warning: Option<String>,
    pub config_file: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
    pub is_default_output: bool,
    pub is_default_input: bool,
    pub max_output_channels: u16,
    pub max_input_channels: u16,
    pub sample_rates: Vec<u32>,
    pub min_buffer_frames: Option<u32>,
    pub max_buffer_frames: Option<u32>,
}

impl From<DeviceInfo> for DeviceDto {
    fn from(device: DeviceInfo) -> Self {
        Self {
            id: device.id,
            name: device.name,
            is_default_output: device.is_default_output,
            is_default_input: device.is_default_input,
            max_output_channels: device.max_output_channels,
            max_input_channels: device.max_input_channels,
            sample_rates: device.sample_rates,
            min_buffer_frames: device.buffer_frames.map(|(min, _)| min),
            max_buffer_frames: device.buffer_frames.map(|(_, max)| max),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendDto {
    pub id: String,
    pub label: String,
    pub devices: Vec<DeviceDto>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiPortDto {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceLists {
    pub backends: Vec<BackendDto>,
    pub midi_inputs: Vec<MidiPortDto>,
    pub midi_error: Option<String>,
    /// Device id meaning "system default" in preferences.
    pub default_device: &'static str,
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

#[tauri::command]
pub fn prefs_get(state: State<'_, EngineState>) -> PrefsSnapshot {
    PrefsSnapshot {
        prefs: lock(&state.prefs).clone(),
        warning: lock(&state.prefs_warning).clone(),
        config_file: state
            .config_dir
            .join(prefs::FILE_NAME)
            .display()
            .to_string(),
    }
}

/// Stores new preferences and applies audio and MIDI changes. Opening a device can take a
/// moment, so this runs off the UI thread.
#[tauri::command]
pub async fn prefs_set(
    state: State<'_, EngineState>,
    prefs: Preferences,
) -> Result<PrefsApplied, String> {
    let state = state.inner();
    thread::scope(|scope| {
        scope
            .spawn(|| state.apply(prefs))
            .join()
            .map_err(|_| "applying preferences panicked".to_string())?
    })
}

/// Lists audio devices per backend and MIDI inputs. Enumeration talks to platform audio APIs
/// that expect their own COM apartment, so it runs on a fresh thread.
#[tauri::command]
pub async fn audio_devices(state: State<'_, EngineState>) -> Result<DeviceLists, String> {
    let state = state.inner();
    thread::scope(|scope| {
        scope
            .spawn(|| {
                let backends = state
                    .engine
                    .backends()
                    .all()
                    .iter()
                    .map(|backend| {
                        let (devices, error) = match backend.devices() {
                            Ok(devices) => {
                                (devices.into_iter().map(DeviceDto::from).collect(), None)
                            }
                            Err(error) => (Vec::new(), Some(error.to_string())),
                        };
                        BackendDto {
                            id: backend.id(),
                            label: backend.label(),
                            devices,
                            error,
                        }
                    })
                    .collect();
                let (midi_inputs, midi_error) = match state.engine.midi().backend().inputs() {
                    Ok(ports) => (
                        ports
                            .into_iter()
                            .map(|port| MidiPortDto {
                                id: port.id,
                                name: port.name,
                            })
                            .collect(),
                        None,
                    ),
                    Err(error) => (Vec::new(), Some(error.to_string())),
                };
                DeviceLists {
                    backends,
                    midi_inputs,
                    midi_error,
                    default_device: DEFAULT_DEVICE,
                }
            })
            .join()
            .map_err(|_| "listing devices panicked".to_string())
    })
}

/// Starts a new round of reopen attempts after the engine gave up on the device.
#[tauri::command]
pub fn audio_retry(state: State<'_, EngineState>) -> EngineStatus {
    state.engine.retry_audio();
    state.status()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_map_to_an_audio_selection() {
        let mut prefs = Preferences::default();
        assert_eq!(audio_selection(&prefs), AudioSelection::default());
        prefs.audio.backend = "wasapi-exclusive".into();
        prefs.audio.output_device = Some("dev".into());
        prefs.audio.sample_rate = Some(96_000);
        prefs.audio.buffer_frames = Some(64);
        let selection = audio_selection(&prefs);
        assert_eq!(selection.backend, "wasapi-exclusive");
        assert_eq!(selection.request.output_device.as_deref(), Some("dev"));
        assert_eq!(selection.request.sample_rate, Some(96_000));
        assert_eq!(selection.request.buffer_frames, Some(64));
        assert_eq!(selection.request.input_device, None);
    }

    #[test]
    fn only_enabled_midi_inputs_are_selected() {
        let prefs = Preferences {
            midi: prefs::MidiPrefs {
                inputs: vec![
                    prefs::MidiInputPref {
                        port: "a".into(),
                        enabled: true,
                        control_surface: true,
                        ..Default::default()
                    },
                    prefs::MidiInputPref {
                        port: "b".into(),
                        enabled: false,
                        ..Default::default()
                    },
                ],
            },
            ..Preferences::default()
        };
        assert_eq!(
            midi_selection(&prefs),
            vec![MidiInputSelection {
                port: "a".into(),
                control_surface: true
            }]
        );
    }

    fn mock_state(dir: &std::path::Path) -> EngineState {
        use ow_io::mock::{MockBackend, MockConfig, MockMidiBackend};
        use ow_io::{AudioBackend, BackendRegistry, MidiBackend};
        use std::sync::Arc;
        let backend = MockBackend::new(MockConfig::default());
        let registry = BackendRegistry::with(vec![Arc::new(backend) as Arc<dyn AudioBackend>]);
        let midi: Arc<dyn MidiBackend> = Arc::new(MockMidiBackend::new(["Keys"]));
        let engine = Engine::with_backends(
            registry,
            midi,
            EngineConfig {
                audio: AudioSelection {
                    backend: "mock".into(),
                    request: StreamRequest::default(),
                },
                ..EngineConfig::default()
            },
        );
        EngineState::from_parts(engine, dir.to_path_buf(), Preferences::default(), None)
    }

    #[test]
    fn apply_stores_valid_changes_and_reports_the_running_device() {
        let tmp = tempfile::tempdir().unwrap();
        let state = mock_state(tmp.path());
        let mut next = Preferences::default();
        next.audio.backend = "mock".into();
        next.audio.buffer_frames = Some(256);
        next.appearance.theme = prefs::Theme::Dark;
        next.midi.inputs = vec![prefs::MidiInputPref {
            port: "Keys".into(),
            name: "Keys".into(),
            ..Default::default()
        }];
        let applied = state.apply(next.clone()).unwrap();
        assert_eq!(applied.audio_error, None);
        assert_eq!(applied.prefs, next);
        assert_eq!(applied.status.audio_state, "running");
        assert_eq!(applied.status.midi.ports.len(), 1);
        assert!(applied.status.midi.ports[0].connected);
        assert_eq!(prefs::load(tmp.path()).prefs, next);
    }

    #[test]
    fn an_audio_device_that_cannot_open_is_reported_and_not_stored() {
        let tmp = tempfile::tempdir().unwrap();
        let state = mock_state(tmp.path());
        let mut next = Preferences::default();
        next.audio.backend = "mock".into();
        next.audio.output_device = Some("unplugged".into());
        next.appearance.theme = prefs::Theme::Light;
        let applied = state.apply(next).unwrap();
        assert!(applied.audio_error.unwrap().contains("unplugged"));
        // Other changes are kept; the audio choice stays as before and audio still runs.
        assert_eq!(applied.prefs.audio, prefs::AudioPrefs::default());
        assert_eq!(applied.prefs.appearance.theme, prefs::Theme::Light);
        assert_eq!(applied.status.audio_state, "running");
        assert_eq!(
            prefs::load(tmp.path()).prefs.audio,
            prefs::AudioPrefs::default()
        );
    }

    #[test]
    fn a_failed_save_leaves_the_engine_and_stored_preferences_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let state = mock_state(tmp.path());
        // A directory where the temp file belongs makes every save fail.
        std::fs::create_dir(tmp.path().join(format!("{}.tmp", prefs::FILE_NAME))).unwrap();
        let before = state.engine.audio_status().selection;
        let mut next = Preferences::default();
        next.audio.backend = "mock".into();
        next.audio.buffer_frames = Some(256);
        let error = state.apply(next).unwrap_err();
        assert!(error.contains("could not save"));
        assert_eq!(state.engine.audio_status().selection, before);
        assert_eq!(*lock(&state.prefs), Preferences::default());
        assert!(!tmp.path().join(prefs::FILE_NAME).exists());
    }

    #[test]
    fn a_rejected_device_restores_the_stored_audio_choice() {
        let tmp = tempfile::tempdir().unwrap();
        let state = mock_state(tmp.path());
        let before = state.engine.audio_status().selection;
        let mut next = Preferences::default();
        next.audio.backend = "mock".into();
        next.audio.output_device = Some("unplugged".into());
        let applied = state.apply(next).unwrap();
        assert!(applied.audio_error.is_some());
        // File, memory and engine agree on the previous audio choice.
        assert_eq!(
            prefs::load(tmp.path()).prefs.audio,
            prefs::AudioPrefs::default()
        );
        assert_eq!(lock(&state.prefs).audio, prefs::AudioPrefs::default());
        assert_eq!(state.engine.audio_status().selection, before);
    }

    #[test]
    fn invalid_preferences_are_rejected_without_touching_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let state = mock_state(tmp.path());
        let mut next = Preferences::default();
        next.audio.buffer_frames = Some(3);
        let error = state.apply(next).unwrap_err();
        assert!(error.contains("buffer size"));
        assert!(!tmp.path().join(prefs::FILE_NAME).exists());
        assert_eq!(state.status().audio_state, "running");
    }
}
