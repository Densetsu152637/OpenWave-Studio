//! User preferences: audio device, MIDI inputs, folders, theme and keyboard shortcuts.
//!
//! Stored as `preferences.json` in the config directory (the portable data folder in portable
//! mode). The file is versioned, validated on load and on every change, and written atomically
//! (temp file, fsync, rename) so a crash cannot leave half a file. A file that fails to parse or
//! validate is moved aside as `preferences.json.corrupt` and defaults are used; a file written by
//! a newer build is moved aside as `preferences.json.newer` so downgrading never destroys it.
//! Mirrored in `src/lib/prefs.ts`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ow_io::{MAX_BUFFER_FRAMES, MIN_BUFFER_FRAMES};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Current preferences format. Bump together with a step in [`migrate`].
pub const PREFS_VERSION: u32 = 1;
/// File name inside the config directory.
pub const FILE_NAME: &str = "preferences.json";

pub const MIN_SAMPLE_RATE: u32 = 8_000;
pub const MAX_SAMPLE_RATE: u32 = 384_000;
const MAX_ID_LEN: usize = 512;
const MAX_PATH_LEN: usize = 4_096;
const MAX_MIDI_INPUTS: usize = 64;
const MAX_PLUGIN_DIRS: usize = 32;
const MAX_SHORTCUTS: usize = 512;
const MAX_ACTION_LEN: usize = 64;
const MAX_ACCELERATOR_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// Audio device choice. Missing values mean "the device default".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioPrefs {
    /// Backend id (`cpal:wasapi`, `wasapi-exclusive`, ...); empty selects the platform default.
    pub backend: String,
    pub output_device: Option<String>,
    pub input_device: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer_frames: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MidiInputPref {
    /// Port id (or name) from the MIDI backend.
    pub port: String,
    /// Name to show when the device is not plugged in.
    pub name: String,
    pub enabled: bool,
    /// Treat the input as a control surface (own shared ring, MIDI-learn).
    pub control_surface: bool,
}

impl Default for MidiInputPref {
    fn default() -> Self {
        Self {
            port: String::new(),
            name: String::new(),
            enabled: true,
            control_surface: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct MidiPrefs {
    pub inputs: Vec<MidiInputPref>,
}

/// Folder overrides; `None` keeps the default from [`crate::paths::Paths`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PathPrefs {
    pub projects_dir: Option<String>,
    /// Extra folders scanned for plugins.
    pub plugin_dirs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AppearancePrefs {
    pub theme: Theme,
}

/// Control of the running app by AI agents through the local MCP endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiPrefs {
    /// Listen on the local MCP socket ("Allow AI control (MCP)"). On by default; only the user
    /// can change it (the MCP tools refuse to).
    pub allow_control: bool,
}

impl Default for AiPrefs {
    fn default() -> Self {
        Self {
            allow_control: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    pub version: u32,
    pub audio: AudioPrefs,
    pub midi: MidiPrefs,
    pub paths: PathPrefs,
    pub appearance: AppearancePrefs,
    pub ai: AiPrefs,
    /// Overrides of the default shortcuts: action id to accelerator (for example
    /// `"transport.play": "Space"`). Empty accelerator unbinds the action.
    pub shortcuts: BTreeMap<String, String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: PREFS_VERSION,
            audio: AudioPrefs::default(),
            midi: MidiPrefs::default(),
            paths: PathPrefs::default(),
            appearance: AppearancePrefs::default(),
            ai: AiPrefs::default(),
            shortcuts: BTreeMap::new(),
        }
    }
}

fn plain(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

fn check_id(what: &str, value: &str, allow_empty: bool) -> Result<(), String> {
    if (!allow_empty && value.is_empty()) || value.len() > MAX_ID_LEN || !plain(value) {
        return Err(format!("{what} is not valid"));
    }
    Ok(())
}

fn check_path(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_PATH_LEN || !plain(value) {
        return Err(format!("{what} is not a valid path"));
    }
    if !Path::new(value).is_absolute() {
        return Err(format!("{what} must be an absolute path"));
    }
    Ok(())
}

impl Preferences {
    /// Checks every field; the first problem is returned as a message for the user.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != PREFS_VERSION {
            return Err(format!("unsupported preferences version {}", self.version));
        }
        let audio = &self.audio;
        check_id("audio backend", &audio.backend, true)?;
        for (what, device) in [
            ("output device", &audio.output_device),
            ("input device", &audio.input_device),
        ] {
            if let Some(device) = device {
                check_id(what, device, false)?;
            }
        }
        if let Some(rate) = audio.sample_rate
            && !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&rate)
        {
            return Err(format!(
                "sample rate must be between {MIN_SAMPLE_RATE} and {MAX_SAMPLE_RATE} Hz"
            ));
        }
        if let Some(frames) = audio.buffer_frames
            && !(MIN_BUFFER_FRAMES..=MAX_BUFFER_FRAMES).contains(&frames)
        {
            return Err(format!(
                "buffer size must be between {MIN_BUFFER_FRAMES} and {MAX_BUFFER_FRAMES} frames"
            ));
        }
        if self.midi.inputs.len() > MAX_MIDI_INPUTS {
            return Err(format!("at most {MAX_MIDI_INPUTS} MIDI inputs"));
        }
        for input in &self.midi.inputs {
            check_id("MIDI port", &input.port, false)?;
            check_id("MIDI port name", &input.name, true)?;
        }
        if let Some(dir) = &self.paths.projects_dir {
            check_path("projects folder", dir)?;
        }
        if self.paths.plugin_dirs.len() > MAX_PLUGIN_DIRS {
            return Err(format!("at most {MAX_PLUGIN_DIRS} plugin folders"));
        }
        for dir in &self.paths.plugin_dirs {
            check_path("plugin folder", dir)?;
        }
        if self.shortcuts.len() > MAX_SHORTCUTS {
            return Err(format!("at most {MAX_SHORTCUTS} shortcuts"));
        }
        for (action, accelerator) in &self.shortcuts {
            let valid_action = !action.is_empty()
                && action.len() <= MAX_ACTION_LEN
                && action
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
            if !valid_action {
                return Err(format!("shortcut action \"{action}\" is not valid"));
            }
            if accelerator.len() > MAX_ACCELERATOR_LEN || !plain(accelerator) {
                return Err(format!("shortcut for \"{action}\" is not valid"));
            }
        }
        Ok(())
    }
}

/// Upgrades an older file's JSON to [`PREFS_VERSION`]. Version 1 is the first, so there is
/// nothing to do yet; a missing version counts as 1. Each future format change adds a step here
/// and a fixture test.
fn migrate(mut value: Value) -> Result<Value, String> {
    let version = match value.get("version") {
        None => 1,
        Some(found) => found
            .as_u64()
            .ok_or_else(|| "preferences version is not a number".to_string())?,
    };
    if version > u64::from(PREFS_VERSION) {
        return Err(format!("written by a newer version ({version})"));
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("version".into(), Value::from(PREFS_VERSION));
    }
    Ok(value)
}

/// What loading found.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub prefs: Preferences,
    /// Why defaults are in use, when the stored file could not be used.
    pub warning: Option<String>,
}

fn move_aside(path: &Path, suffix: &str) -> Option<PathBuf> {
    let mut name = path.file_name()?.to_os_string();
    name.push(suffix);
    let target = path.with_file_name(name);
    fs::rename(path, &target).ok().map(|()| target)
}

/// Loads preferences from `config_dir`. A missing file gives defaults silently; an unusable
/// one gives defaults and a warning, and the old file is kept under a different name.
pub fn load(config_dir: &Path) -> Loaded {
    let path = config_dir.join(FILE_NAME);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Loaded {
                prefs: Preferences::default(),
                warning: None,
            };
        }
        Err(error) => {
            return Loaded {
                prefs: Preferences::default(),
                warning: Some(format!("Could not read the preferences file: {error}")),
            };
        }
    };
    let parsed = String::from_utf8(bytes)
        .map_err(|error| format!("not valid UTF-8: {error}"))
        .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|error| error.to_string()))
        .and_then(migrate)
        .and_then(|value| {
            serde_json::from_value::<Preferences>(value).map_err(|error| error.to_string())
        })
        .and_then(|prefs| prefs.validate().map(|()| prefs));
    match parsed {
        Ok(prefs) => Loaded {
            prefs,
            warning: None,
        },
        Err(reason) => {
            let newer = reason.starts_with("written by a newer");
            let kept = move_aside(&path, if newer { ".newer" } else { ".corrupt" });
            let kept =
                kept.map_or_else(String::new, |p| format!(" It was kept as {}.", p.display()));
            Loaded {
                prefs: Preferences::default(),
                warning: Some(format!(
                    "The preferences file could not be used ({reason}); defaults are in use.{kept}"
                )),
            }
        }
    }
}

/// Writes `prefs` atomically. Validates first so an invalid value never reaches disk.
pub fn save(config_dir: &Path, prefs: &Preferences) -> Result<(), String> {
    prefs.validate()?;
    fs::create_dir_all(config_dir)
        .map_err(|error| format!("could not create {}: {error}", config_dir.display()))?;
    let path = config_dir.join(FILE_NAME);
    let temp = config_dir.join(format!("{FILE_NAME}.tmp"));
    let json = serde_json::to_vec_pretty(prefs).map_err(|error| error.to_string())?;
    let write = || -> io::Result<()> {
        let mut file = File::create(&temp)?;
        file.write_all(&json)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &path)
    };
    write().map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("could not save preferences: {error}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[cfg(windows)]
    const ABS: &str = "C:\\Music\\OpenWave";
    #[cfg(not(windows))]
    const ABS: &str = "/home/user/Music/OpenWave";

    fn sample() -> Preferences {
        Preferences {
            audio: AudioPrefs {
                backend: "cpal:wasapi".into(),
                output_device: Some("wasapi:{0.0.0.00000000}.{abc}".into()),
                input_device: None,
                sample_rate: Some(48_000),
                buffer_frames: Some(128),
            },
            midi: MidiPrefs {
                inputs: vec![MidiInputPref {
                    port: "Keys 1".into(),
                    name: "Keys".into(),
                    enabled: true,
                    control_surface: true,
                }],
            },
            paths: PathPrefs {
                projects_dir: Some(ABS.into()),
                plugin_dirs: Vec::new(),
            },
            appearance: AppearancePrefs {
                theme: Theme::Light,
            },
            shortcuts: BTreeMap::from([("transport.play".to_string(), "Space".to_string())]),
            ..Preferences::default()
        }
    }

    #[test]
    fn defaults_are_valid() {
        Preferences::default().validate().unwrap();
        assert_eq!(Preferences::default().version, PREFS_VERSION);
    }

    #[test]
    fn missing_file_gives_defaults_without_a_warning() {
        let tmp = dir();
        let loaded = load(tmp.path());
        assert_eq!(loaded.prefs, Preferences::default());
        assert!(loaded.warning.is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = dir();
        let prefs = sample();
        save(tmp.path(), &prefs).unwrap();
        let loaded = load(tmp.path());
        assert_eq!(loaded.prefs, prefs);
        assert!(loaded.warning.is_none());
        assert!(!tmp.path().join("preferences.json.tmp").exists());
    }

    #[test]
    fn save_creates_the_config_directory() {
        let tmp = dir();
        let nested = tmp.path().join("a").join("b");
        save(&nested, &Preferences::default()).unwrap();
        assert!(nested.join(FILE_NAME).is_file());
    }

    #[test]
    fn unknown_fields_and_missing_fields_are_tolerated() {
        let tmp = dir();
        fs::write(
            tmp.path().join(FILE_NAME),
            r#"{"version":1,"appearance":{"theme":"dark","future":1},"extra":true}"#,
        )
        .unwrap();
        let loaded = load(tmp.path());
        assert!(loaded.warning.is_none());
        assert_eq!(loaded.prefs.appearance.theme, Theme::Dark);
        assert_eq!(loaded.prefs.audio, AudioPrefs::default());
    }

    #[test]
    fn a_missing_version_is_treated_as_version_one() {
        let tmp = dir();
        fs::write(
            tmp.path().join(FILE_NAME),
            r#"{"appearance":{"theme":"light"}}"#,
        )
        .unwrap();
        let loaded = load(tmp.path());
        assert!(loaded.warning.is_none());
        assert_eq!(loaded.prefs.version, PREFS_VERSION);
        assert_eq!(loaded.prefs.appearance.theme, Theme::Light);
    }

    #[test]
    fn corrupt_files_are_kept_aside_and_defaults_used() {
        let tmp = dir();
        let path = tmp.path().join(FILE_NAME);
        fs::write(&path, "{ not json").unwrap();
        let loaded = load(tmp.path());
        assert_eq!(loaded.prefs, Preferences::default());
        assert!(loaded.warning.unwrap().contains("defaults are in use"));
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(tmp.path().join("preferences.json.corrupt")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn a_file_that_is_not_utf8_is_kept_aside_and_defaults_used() {
        let tmp = dir();
        let path = tmp.path().join(FILE_NAME);
        fs::write(&path, [0xff, 0xfe]).unwrap();
        let loaded = load(tmp.path());
        assert_eq!(loaded.prefs, Preferences::default());
        assert!(loaded.warning.unwrap().contains("defaults are in use"));
        assert!(!path.exists());
        assert_eq!(
            fs::read(tmp.path().join("preferences.json.corrupt")).unwrap(),
            [0xff, 0xfe]
        );
    }

    #[test]
    fn invalid_values_in_a_stored_file_count_as_corrupt() {
        let tmp = dir();
        fs::write(
            tmp.path().join(FILE_NAME),
            r#"{"version":1,"audio":{"bufferFrames":5}}"#,
        )
        .unwrap();
        let loaded = load(tmp.path());
        assert_eq!(loaded.prefs.audio.buffer_frames, None);
        assert!(loaded.warning.unwrap().contains("buffer size"));
        assert!(tmp.path().join("preferences.json.corrupt").exists());
    }

    #[test]
    fn a_newer_file_is_preserved_untouched_under_another_name() {
        let tmp = dir();
        let text = r#"{"version":99,"somethingNew":{}}"#;
        fs::write(tmp.path().join(FILE_NAME), text).unwrap();
        let loaded = load(tmp.path());
        assert_eq!(loaded.prefs, Preferences::default());
        assert!(loaded.warning.unwrap().contains("newer version"));
        assert_eq!(
            fs::read_to_string(tmp.path().join("preferences.json.newer")).unwrap(),
            text
        );
    }

    #[test]
    fn validation_rejects_out_of_range_and_malformed_values() {
        let reject = |change: &dyn Fn(&mut Preferences), expect: &str| {
            let mut prefs = sample();
            change(&mut prefs);
            let error = prefs.validate().unwrap_err();
            assert!(error.contains(expect), "{error} should mention {expect}");
        };
        reject(&|p| p.audio.sample_rate = Some(100), "sample rate");
        reject(&|p| p.audio.sample_rate = Some(10_000_000), "sample rate");
        reject(&|p| p.audio.buffer_frames = Some(8), "buffer size");
        reject(&|p| p.audio.buffer_frames = Some(100_000), "buffer size");
        reject(
            &|p| p.audio.output_device = Some(String::new()),
            "output device",
        );
        reject(&|p| p.audio.backend = "a\nb".into(), "backend");
        reject(
            &|p| p.paths.projects_dir = Some("relative/dir".into()),
            "absolute",
        );
        reject(
            &|p| p.paths.plugin_dirs = vec!["rel".into()],
            "plugin folder",
        );
        reject(&|p| p.midi.inputs[0].port = String::new(), "MIDI port");
        reject(
            &|p| {
                p.shortcuts.insert("bad action!".into(), "X".into());
            },
            "action",
        );
        reject(&|p| p.version = 7, "version");
        let mut many = sample();
        many.midi.inputs = vec![
            MidiInputPref {
                port: "x".into(),
                ..Default::default()
            };
            65
        ];
        assert!(many.validate().is_err());
    }

    #[test]
    fn save_refuses_invalid_preferences_and_leaves_the_old_file() {
        let tmp = dir();
        save(tmp.path(), &sample()).unwrap();
        let mut bad = sample();
        bad.audio.buffer_frames = Some(1);
        assert!(save(tmp.path(), &bad).is_err());
        assert_eq!(load(tmp.path()).prefs, sample());
    }
}
