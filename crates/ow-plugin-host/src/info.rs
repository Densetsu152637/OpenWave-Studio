//! What the scanner learns about a plugin file, and the scan cache's record format.
//!
//! Everything here is plain serialisable data with no format-specific types, so CLAP and VST3
//! fill the same records: [`PluginInfo`] is one plugin, [`FileRecord`] is one scanned
//! file with its outcome, [`ScanCache`] is the `plugin-cache.json` file.
//!
//! A plugin's `kind` is what projects store in `Device::kind`: `clap:<plugin id>` or
//! `vst3:<class id>` (the class id as 32 hexadecimal digits).

use serde::{Deserialize, Serialize};

/// Version of the scan record format. A cache file with another version is ignored (and
/// rebuilt by the next scan).
pub const RECORD_VERSION: u32 = 2;

/// A plugin format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// CLAP.
    Clap,
    /// VST3.
    Vst3,
}

impl Format {
    /// The prefix of `Device::kind` values (`clap`, `vst3`).
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Clap => "clap",
            Self::Vst3 => "vst3",
        }
    }

    /// The tag of `StateChunk::format` for this format's state.
    pub const fn state_tag(self) -> &'static str {
        self.prefix()
    }
}

/// The `Device::kind` of plugin `id` in `format`.
pub fn kind_of(format: Format, id: &str) -> String {
    format!("{}:{id}", format.prefix())
}

/// Splits `Device::kind` into a plugin format and the plugin's identifier; `None` for built-in
/// kinds and anything unknown.
pub fn parse_kind(kind: &str) -> Option<(Format, &str)> {
    let (prefix, id) = kind.split_once(':')?;
    let format = match prefix {
        "clap" => Format::Clap,
        "vst3" => Format::Vst3,
        _ => return None,
    };
    (!id.is_empty()).then_some((format, id))
}

/// True when `kind` names a hosted plugin rather than a built-in device.
pub fn is_plugin_kind(kind: &str) -> bool {
    parse_kind(kind).is_some()
}

/// One parameter a plugin exposes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    /// The plugin's own, stable parameter id.
    pub id: u32,
    /// Name shown next to the control.
    pub name: String,
    /// The group the plugin puts it in (may be empty).
    pub module: String,
    /// Smallest plain value.
    pub min: f64,
    /// Largest plain value.
    pub max: f64,
    /// Value a fresh instance starts at.
    pub default: f64,
    /// True for a parameter that only takes whole values.
    pub stepped: bool,
    /// Option labels of an enumeration (the value is the index), otherwise empty.
    pub choices: Vec<String>,
    /// The plugin asks hosts not to show it.
    pub hidden: bool,
    /// The plugin never accepts a value for it.
    pub read_only: bool,
}

impl ParamInfo {
    /// `value` limited to the parameter's range (rounded when stepped); a value that is not a
    /// number becomes the default.
    pub fn clamp(&self, value: f64) -> f64 {
        if !value.is_finite() {
            return self.default;
        }
        let value = value.clamp(self.min, self.max);
        if self.stepped { value.round() } else { value }
    }
}

/// One plugin in a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// The format.
    pub format: Format,
    /// `Device::kind`: `clap:<id>` or `vst3:<class id>`.
    pub kind: String,
    /// The plugin's own identifier.
    pub id: String,
    /// Name shown in menus.
    pub name: String,
    /// Vendor, possibly empty.
    pub vendor: String,
    /// Version text, possibly empty.
    pub version: String,
    /// The plugin's feature tags (CLAP `features`).
    pub features: Vec<String>,
    /// True for a plugin that makes sound from notes (a track's instrument).
    pub instrument: bool,
    /// Channels of the main audio input (0 when it has none).
    pub audio_inputs: u32,
    /// Channels of the main audio output.
    pub audio_outputs: u32,
    /// True when the plugin has a note input port.
    pub note_input: bool,
    /// Latency the plugin reported while the scanner had it active, in frames.
    pub latency: u32,
    /// The parameters.
    pub params: Vec<ParamInfo>,
    /// The plugin has an editor window this platform's hosting can show (whether the platform
    /// can show one at all is [`crate::editor::AVAILABLE`]).
    #[serde(default)]
    pub editor: bool,
    /// The file the plugin was found in.
    pub path: String,
}

impl PluginInfo {
    /// The parameter with `id`.
    pub fn param(&self, id: u32) -> Option<&ParamInfo> {
        self.params.iter().find(|param| param.id == id)
    }
}

/// How scanning a file ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ScanStatus {
    /// The file was loaded and its plugins listed.
    Ok,
    /// The scanner process died (a crashing plugin). The file is blacklisted until it changes.
    Crashed {
        /// How the process ended, as text.
        detail: String,
    },
    /// The scanner did not finish in time. The file is blacklisted until it changes.
    TimedOut,
    /// The file could not be loaded or has no plugin entry; not a crash.
    Failed {
        /// Why.
        message: String,
    },
}

impl ScanStatus {
    /// True for [`Ok`](Self::Ok).
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// One scanned file and what came of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
    /// Absolute path of the plugin file or bundle.
    pub path: String,
    /// Size in bytes (for a bundle, of its executable).
    pub size: u64,
    /// Modification time in milliseconds since the Unix epoch (for a bundle, of its
    /// executable).
    pub modified_ms: u64,
    /// The outcome.
    #[serde(flatten)]
    pub status: ScanStatus,
    /// The plugins found (empty unless the status is ok).
    pub plugins: Vec<PluginInfo>,
}

/// The `plugin-cache.json` file: every scanned file, keyed by path, size and modification time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanCache {
    /// [`RECORD_VERSION`].
    pub version: u32,
    /// The records.
    pub files: Vec<FileRecord>,
}

impl Default for ScanCache {
    fn default() -> Self {
        Self {
            version: RECORD_VERSION,
            files: Vec::new(),
        }
    }
}

impl ScanCache {
    /// The record for `path` when it still describes a file of that size and modification time.
    pub fn fresh(&self, path: &str, size: u64, modified_ms: u64) -> Option<&FileRecord> {
        self.files
            .iter()
            .find(|f| f.path == path && f.size == size && f.modified_ms == modified_ms)
    }
}

/// What the `ow-scan` child prints on standard output for one file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScanOutput {
    /// The file loaded.
    Plugins {
        /// [`RECORD_VERSION`].
        version: u32,
        /// What is in it.
        plugins: Vec<PluginInfo>,
    },
    /// The file did not load (a normal failure, not a crash).
    Error {
        /// Why.
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_builtin_kinds_are_not_plugins() {
        assert_eq!(kind_of(Format::Clap, "com.x.y"), "clap:com.x.y");
        assert_eq!(parse_kind("clap:com.x.y"), Some((Format::Clap, "com.x.y")));
        assert_eq!(parse_kind("vst3:AB"), Some((Format::Vst3, "AB")));
        assert_eq!(parse_kind("builtin.synth"), None);
        assert_eq!(parse_kind("clap:"), None);
        assert_eq!(parse_kind("au:x"), None);
        assert!(is_plugin_kind("clap:a"));
        assert!(!is_plugin_kind("builtin.eq"));
    }

    #[test]
    fn a_stepped_parameter_rounds_and_nan_becomes_the_default() {
        let param = ParamInfo {
            id: 1,
            name: "p".into(),
            module: String::new(),
            min: 0.0,
            max: 3.0,
            default: 1.0,
            stepped: true,
            choices: Vec::new(),
            hidden: false,
            read_only: false,
        };
        assert_eq!(param.clamp(1.6), 2.0);
        assert_eq!(param.clamp(9.0), 3.0);
        assert_eq!(param.clamp(f64::NAN), 1.0);
    }

    #[test]
    fn file_records_serialise_with_a_flat_status() {
        let record = FileRecord {
            path: "/p/x.clap".into(),
            size: 5,
            modified_ms: 9,
            status: ScanStatus::Crashed {
                detail: "exit 3".into(),
            },
            plugins: Vec::new(),
        };
        let text = serde_json::to_string(&record).unwrap();
        assert!(text.contains(r#""status":"crashed""#), "{text}");
        let back: FileRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(back, record);
    }
}
