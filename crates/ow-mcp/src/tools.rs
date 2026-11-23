//! The tool catalog: names, descriptions and input schemas, and the typed [`ToolCall`] a request
//! turns into.
//!
//! Every tool is declared once in the `tools!` table below, which generates the [`ToolCall`]
//! enum, the argument parsing and the catalog, so a tool cannot exist without a schema or the
//! other way around. Schemas come from `schemars` over the argument structs; the `edit` tool's
//! schema therefore carries the full `EditCommand` shape from `ow-model`.
//!
//! Argument field names are snake_case, like the model's own edit commands.

use ow_model::{EditCommand, MixerRef};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// What a tool takes when it takes nothing.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

/// Arguments of `edit`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditArgs {
    /// The edits, applied in order. One command is one undo step. Several commands are applied
    /// as one transaction (one undo step named `label`): if any command is refused, every
    /// command of the call is reverted and the error names the refused one.
    #[schemars(length(min = 1))]
    pub commands: Vec<EditCommand>,
    /// Undo-menu name of the step when `commands` holds more than one command.
    #[serde(default)]
    pub label: Option<String>,
}

/// Arguments of `alloc_ids`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AllocIdsArgs {
    /// How many ids to reserve (at most 1024).
    pub count: u32,
}

/// What `transport` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransportAction {
    /// Start playback from the current position.
    Play,
    /// Stop playback (also ends a recording and adds the takes); the position holds.
    Stop,
    /// Move the position to `beats`.
    Locate,
    /// Report whether the engine runs, the position and the audio state.
    Status,
}

/// Arguments of `transport`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransportArgs {
    /// What to do.
    pub action: TransportAction,
    /// Position in beats (quarter notes from the start) for `locate`.
    #[serde(default)]
    pub beats: Option<f64>,
}

/// What `project` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAction {
    /// Start an empty project.
    New,
    /// Open the `.owave` bundle at `path`.
    Open,
    /// Save to where the project came from.
    Save,
    /// Save to the bundle `path` (`.owave` is added when missing).
    SaveAs,
}

/// Arguments of `project`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectArgs {
    /// What to do.
    pub action: ProjectAction,
    /// The bundle path for `open` and `save_as`.
    #[serde(default)]
    pub path: Option<String>,
    /// Required (true) by `new` and `open` when the open project has unsaved changes, which
    /// they discard.
    #[serde(default)]
    pub discard_changes: bool,
    /// Required (true) by `save_as` when `path` already exists, which it replaces.
    #[serde(default)]
    pub overwrite: bool,
}

/// Arguments of `import_audio`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportAudioArgs {
    /// Absolute path of the audio file (WAV, FLAC, MP3, OGG and the other formats the app reads).
    pub path: String,
}

/// What `record` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecordAction {
    /// Arm or disarm an audio track for recording.
    Arm,
    /// Start recording on every armed track from the current position.
    Start,
    /// Stop recording (and playback); the takes become clips.
    Stop,
}

/// Arguments of `record`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordArgs {
    /// What to do.
    pub action: RecordAction,
    /// The audio track id for `arm`.
    #[serde(default)]
    pub track: Option<u64>,
    /// For `arm`: true arms the track (the default), false disarms it.
    #[serde(default)]
    pub armed: Option<bool>,
}

/// Arguments of `analyze`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnalyzeArgs {
    /// The strip to hear after its fader: `{"type":"track","id":N}`, a bus, or `{"type":"master"}`.
    /// Omit for the master (the whole mix).
    #[serde(default)]
    pub strip: Option<MixerRef>,
    /// First beat to analyse; omit for the start of the project.
    #[serde(default)]
    pub start_beat: Option<f64>,
    /// Beat to stop at (exclusive); omit for the end of the last clip plus the export tail.
    #[serde(default)]
    pub end_beat: Option<f64>,
}

/// Loudness normalisation of an export.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NormalizeArgs {
    /// Integrated loudness to reach, in LUFS (for example -14).
    pub target_lufs: f64,
    /// True-peak ceiling in dBTP (for example -1): the gain is lowered when reaching the target
    /// would exceed it. It is a plain gain; nothing is limited or clipped.
    pub true_peak_ceiling_db: f64,
}

/// Arguments of `export_wav`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportWavArgs {
    /// Absolute path of the WAV file to write (`.wav` is added when there is no extension).
    pub path: String,
    /// Scale the mix to a loudness first; omit to export it as rendered.
    #[serde(default)]
    pub normalize: Option<NormalizeArgs>,
    /// Required (true) when the file already exists, which it replaces.
    #[serde(default)]
    pub overwrite: bool,
}

/// What `audio_settings` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioAction {
    /// List the audio backends and devices and the MIDI inputs.
    ListDevices,
    /// Report the stored preferences.
    Get,
    /// Change preferences (`preferences` is merged into the stored ones).
    Set,
    /// Start a new round of reopen attempts after the engine gave up on the audio device.
    Retry,
    /// Turn the 440 Hz test tone on or off (`on`).
    TestTone,
}

/// Arguments of `audio_settings`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioSettingsArgs {
    /// What to do.
    pub action: AudioAction,
    /// For `set`: the preference fields to change, in the shape `get` reports (camelCase), for
    /// example `{"audio":{"backend":"","outputDevice":"Speakers","sampleRate":48000,"bufferFrames":256}}`.
    /// Objects are merged field by field; `null` clears an optional field. The AI control
    /// setting cannot be changed from here.
    #[serde(default)]
    pub preferences: Option<Value>,
    /// For `test_tone`: true starts it, false stops it.
    #[serde(default)]
    pub on: Option<bool>,
}

/// What `plugin_editor` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditorAction {
    /// Open the plugin's own editor window.
    Open,
    /// Close it.
    Close,
}

/// Arguments of `scan_plugins`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScanPluginsArgs {
    /// Scan every plugin file again, also those that did not change since the last scan (and
    /// those that crashed it before). By default unchanged files are answered from the cache.
    #[serde(default)]
    pub force: bool,
}

/// Arguments of `plugin_editor`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginEditorArgs {
    /// Open or close.
    pub action: EditorAction,
    /// The device id of the hosted plugin.
    pub device: u64,
}

/// One tool as `tools/list` shows it.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// The tool name.
    pub name: &'static str,
    /// What it does, for the model.
    pub description: &'static str,
    /// JSON Schema of its arguments (an object schema).
    pub input_schema: Value,
}

fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null)
}

/// Why a request could not become a [`ToolCall`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// No tool has that name.
    Unknown(String),
    /// The arguments do not fit the tool's schema.
    Arguments(String),
}

macro_rules! tools {
    ($($variant:ident($args:ty) = $name:literal, $description:literal;)*) => {
        /// A tool call with typed arguments.
        #[derive(Debug, Clone)]
        pub enum ToolCall {
            $(
                #[doc = $description]
                $variant($args),
            )*
        }

        /// Every tool name, in catalog order.
        pub const TOOL_NAMES: &[&str] = &[$($name),*];

        impl ToolCall {
            /// Turns a `tools/call` request into a typed call.
            pub fn parse(name: &str, arguments: Value) -> Result<Self, ParseError> {
                let arguments = if arguments.is_null() {
                    Value::Object(serde_json::Map::new())
                } else {
                    arguments
                };
                match name {
                    $(
                        $name => serde_json::from_value::<$args>(arguments)
                            .map(Self::$variant)
                            .map_err(|error| ParseError::Arguments(error.to_string())),
                    )*
                    other => Err(ParseError::Unknown(other.to_owned())),
                }
            }

            /// The tool's name.
            pub fn name(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $name,)*
                }
            }
        }

        /// The catalog `tools/list` answers with.
        pub fn catalog() -> Vec<ToolSpec> {
            vec![$(
                ToolSpec {
                    name: $name,
                    description: $description,
                    input_schema: schema_of::<$args>(),
                },
            )*]
        }
    };
}

tools! {
    GetProject(NoArgs) = "get_project",
        "Read the whole open project as the UI shows it: settings, tempo and time-signature maps, tracks (with their strip settings: volume, pan, mute, solo, output routing, sends), buses, the master strip, clips (MIDI clips with their notes), patterns, the media pool, devices with their current parameter values, automation lanes, markers. Also the history state (can_undo, can_redo, dirty, armed_tracks, missing_media, engine_error) and the saved project path. All ids are numbers, unique across every kind of entity.";
    GetDeviceTypes(NoArgs) = "get_device_types",
        "List the device kinds that can be added as a track's instrument or as an insert effect: each kind's id (the `kind` of a Device), its name and its parameters (id, name, range, default, unit, scale, choice labels). Parameter values are set with the edit tool's set_device_param command, for built-in devices and third-party plugins alike. `builtin` lists the built-in devices and `plugins` the scanned CLAP plugins (kind `clap:<plugin id>`; instruments are added as a MIDI track's instrument, effects as inserts). A plugin's state is saved with the project.";
    Edit(EditArgs) = "edit",
        "Change the project with edit commands, exactly as the UI does (the UI updates live and every call is an undo step). Anything a user can do to the project is an edit command: add, remove and move tracks and buses, add and move clips, notes, devices and sends, set volume, pan, mute, solo, output routing, tempo, markers and project settings. New entities need ids: reserve them with alloc_ids first and use them in the command (insert_track takes a bundle with the track, its clips and its devices). Read get_project to see ids and current values. Pass several commands to apply them atomically as one undo step.";
    AllocIds(AllocIdsArgs) = "alloc_ids",
        "Reserve fresh entity ids for the entities that your next edit commands create (tracks, buses, clips, devices, patterns, markers, lanes). Ids are unique across all kinds and never reused.";
    Undo(NoArgs) = "undo",
        "Revert the latest undo step (an edit call, or a UI action). Fails when there is nothing to undo or a transaction is open.";
    Redo(NoArgs) = "redo",
        "Reapply the latest undone step.";
    Transport(TransportArgs) = "transport",
        "Control playback: play, stop, locate to a position in beats, or status (engine and audio device state, position in frames and beats, whether the position is moving). Playback is audible on the user's audio device.";
    Project(ProjectArgs) = "project",
        "Project files: new, open and save, or save_as (a project is a `.owave` bundle directory). new and open discard unsaved changes, so they need discard_changes=true when the project is dirty (get_project shows dirty). save_as onto an existing path needs overwrite=true. This reads and writes the user's files with the user's own file access.";
    ImportAudio(ImportAudioArgs) = "import_audio",
        "Decode an audio file, convert it to the project's sample rate and add it to the media pool. Answers the media id; place it on the timeline by adding an audio clip that references the media id with the edit tool. The same content is not added twice.";
    Record(RecordArgs) = "record",
        "Record audio: arm or disarm an audio track, start recording on the armed tracks from the current position, stop. The takes become clips when recording stops. Needs an audio input device (see audio_settings).";
    Analyze(AnalyzeArgs) = "analyze",
        "Render the project offline (the master, or one strip after its fader; the whole project or a beat range) and measure it: integrated, short-term maximum and momentary maximum loudness (LUFS, ITU-R BS.1770-4), true peak (dBTP), sample peak, RMS, crest factor, stereo correlation and eight frequency-band energies. Use it to check a mix and to find how far to move the levels.";
    ExportWav(ExportWavArgs) = "export_wav",
        "Render the project to a 32-bit float stereo WAV file, optionally normalised to a target integrated loudness with a true-peak ceiling (a plain gain). Answers the file written, what normalisation did and the measurements of what was written. Writes a file with the user's own file access; an existing file needs overwrite=true.";
    AudioSettings(AudioSettingsArgs) = "audio_settings",
        "Audio and MIDI device settings: list the backends, devices and MIDI inputs; get or set the preferences (audio backend, output and input device, sample rate, buffer size, MIDI inputs); retry a failed audio device; toggle the test tone.";
    ScanPlugins(ScanPluginsArgs) = "scan_plugins",
        "Scan for third-party CLAP plugins: the standard plugin folders of this OS, the CLAP_PATH folders and the extra plugin folders of the preferences (audio_settings set `{\"paths\":{\"pluginDirs\":[\"/absolute/folder\"]}}`). Each plugin file is loaded in a separate process, so a plugin that crashes or hangs is skipped and reported, not fatal; files that did not change are answered from the cache unless force is true. Answers the usable plugins (their `kind` is what a device's kind is set to) and the files that failed.";
    ListPlugins(NoArgs) = "list_plugins",
        "List the plugins the last scan found and the plugin files it had to skip (crashed, timed out or failed to load), without scanning again.";
    PluginEditor(PluginEditorArgs) = "plugin_editor",
        "Open or close a hosted plugin's own editor window. Not available yet: the answer says so; set the plugin's parameters with the edit tool instead.";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_entry_has_an_object_schema() {
        let catalog = catalog();
        assert_eq!(catalog.len(), TOOL_NAMES.len());
        for tool in &catalog {
            assert_eq!(
                tool.input_schema["type"], "object",
                "{} must take an object",
                tool.name
            );
            assert!(!tool.description.is_empty());
        }
    }

    #[test]
    fn arguments_parse_and_unknown_fields_are_refused() {
        let call = ToolCall::parse(
            "transport",
            serde_json::json!({"action": "locate", "beats": 4}),
        )
        .unwrap();
        assert_eq!(call.name(), "transport");
        assert!(matches!(
            ToolCall::parse(
                "transport",
                serde_json::json!({"action": "play", "bogus": 1})
            ),
            Err(ParseError::Arguments(_))
        ));
        assert!(matches!(
            ToolCall::parse("get_project", Value::Null),
            Ok(ToolCall::GetProject(_))
        ));
        assert_eq!(
            ToolCall::parse("nope", Value::Null).unwrap_err(),
            ParseError::Unknown("nope".into())
        );
    }

    #[test]
    fn the_edit_schema_lists_every_edit_command() {
        use strum::VariantNames as _;
        let edit = catalog()
            .into_iter()
            .find(|tool| tool.name == "edit")
            .unwrap();
        let text = edit.input_schema.to_string();
        for variant in EditCommand::VARIANTS {
            assert!(
                text.contains(&format!("\"{variant}\"")),
                "the edit schema lacks `{variant}`"
            );
        }
    }
}
