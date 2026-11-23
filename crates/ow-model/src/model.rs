//! The project data model.
//!
//! A [`Project`] is plain data: the control thread owns the authoritative copy and changes it only
//! through [`EditCommand`](crate::EditCommand)s (see [`Editor`](crate::Editor)), so every change
//! has an inverse. Fields are public for reading; mutating them directly bypasses undo and the
//! invariants [`Project::validate`] enforces.
//!
//! Positions and lengths are `f64` quarter-note beats ([`crate::time`]); positions inside a MIDI
//! clip or pattern are relative to its start.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{BusId, ClipId, DeviceId, LaneId, MarkerId, MediaId, PatternId, TrackId};
use crate::keyed;
use crate::time::{TempoMap, TimeSignatureMap};
use crate::validate::MAX_ID;

/// Something stored in a keyed collection (serialized as a list, kept in an ordered map).
pub trait Keyed {
    /// The id type.
    type Id: Copy + Ord + std::fmt::Debug;
    /// This entity's id.
    fn id(&self) -> Self::Id;
}

/// Reference to a mixer strip: a track, a bus or the master.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MixerRef {
    /// A track's strip.
    Track(TrackId),
    /// A bus's strip.
    Bus(BusId),
    /// The master strip.
    Master,
}

/// Where a strip's output goes.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    /// The master strip.
    #[default]
    Master,
    /// A bus.
    Bus(BusId),
}

/// A send from a strip to a bus.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AuxSend {
    /// Destination bus.
    pub target: BusId,
    /// Send level in dB.
    pub level_db: f64,
    /// Tapped before the fader (true) or after it.
    pub pre_fader: bool,
}

/// The scalar settings of a strip.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StripSettings {
    /// Fader gain in dB.
    pub gain_db: f64,
    /// Pan, `-1.0` (left) to `1.0` (right).
    pub pan: f64,
    /// Muted.
    pub mute: bool,
    /// Soloed.
    pub solo: bool,
    /// Never muted by another strip's solo.
    pub solo_safe: bool,
    /// Polarity inverted.
    pub invert_phase: bool,
    /// Output routing (always `Master` for the master strip).
    pub output: Output,
}

impl Default for StripSettings {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            solo_safe: false,
            invert_phase: false,
            output: Output::Master,
        }
    }
}

/// A mixer strip: settings, sends and the insert chain.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Strip {
    /// Fader, pan, mute, solo and routing.
    pub settings: StripSettings,
    /// Sends to buses (empty on the master).
    #[serde(default)]
    pub sends: Vec<AuxSend>,
    /// Insert effects in signal order. Each device appears in exactly one slot in the project.
    #[serde(default)]
    pub inserts: Vec<DeviceId>,
}

/// What a track carries.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// Audio clips.
    Audio,
    /// MIDI clips and pattern instances, driving the track's instrument.
    Midi,
}

/// A track.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// Id.
    pub id: TrackId,
    /// Display name.
    pub name: String,
    /// Color as `0xRRGGBB`.
    pub color: u32,
    /// Audio or MIDI.
    pub kind: TrackKind,
    /// Mixer strip.
    pub strip: Strip,
    /// The instrument device (MIDI tracks only).
    #[serde(default)]
    pub instrument: Option<DeviceId>,
}

/// A bus or group channel: tracks and other buses route or send into it.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bus {
    /// Id.
    pub id: BusId,
    /// Display name.
    pub name: String,
    /// Color as `0xRRGGBB`.
    pub color: u32,
    /// Mixer strip.
    pub strip: Strip,
}

/// A MIDI note, positioned relative to its clip or pattern.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// Start in beats.
    pub start: f64,
    /// Length in beats.
    pub length: f64,
    /// MIDI key, 0 to 127.
    pub key: u8,
    /// Velocity, 1 to 127.
    pub velocity: u8,
}

/// A MIDI controller event, positioned relative to its clip.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Controller {
    /// Position in beats.
    pub beat: f64,
    /// Controller number, 0 to 127.
    pub controller: u8,
    /// Value, 0 to 127.
    pub value: u8,
}

/// What a clip plays.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipContent {
    /// A region of a media pool item (audio tracks).
    Audio {
        /// Source media.
        media: MediaId,
        /// First source frame played (at the media's own sample rate).
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        source_offset: u64,
        /// Clip gain in dB.
        gain_db: f64,
        /// Fade-in length in beats.
        fade_in: f64,
        /// Fade-out length in beats.
        fade_out: f64,
    },
    /// MIDI data owned by the clip (MIDI tracks).
    Midi {
        /// Notes.
        notes: Vec<Note>,
        /// Controller events.
        #[serde(default)]
        controllers: Vec<Controller>,
    },
    /// An instance of a shared pattern (MIDI tracks).
    Pattern {
        /// The pattern played.
        pattern: PatternId,
    },
}

/// A clip on a track.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    /// Id.
    pub id: ClipId,
    /// The track it sits on; the content kind must suit the track kind.
    pub track: TrackId,
    /// Display name.
    pub name: String,
    /// Start on the timeline, in beats.
    pub start: f64,
    /// Length in beats.
    pub length: f64,
    /// Muted.
    #[serde(default)]
    pub muted: bool,
    /// What it plays.
    pub content: ClipContent,
}

/// A pattern shared by its instances: editing it changes every instance.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pattern {
    /// Id.
    pub id: PatternId,
    /// Display name.
    pub name: String,
    /// Color as `0xRRGGBB`.
    pub color: u32,
    /// Length in beats (the instance loops it).
    pub length: f64,
    /// Notes, relative to the pattern start.
    #[serde(default)]
    pub notes: Vec<Note>,
}

/// An imported or recorded audio file inside the bundle.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaItem {
    /// Id.
    pub id: MediaId,
    /// Display name.
    pub name: String,
    /// Path relative to the bundle, always under `audio/` with `/` separators.
    pub path: String,
    /// Content hash (alphanumeric, normally hexadecimal) the file name derives from.
    pub hash: String,
    /// Sample rate of the file.
    pub sample_rate: u32,
    /// Channel count of the file.
    pub channels: u16,
    /// Length in frames.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub frames: u64,
}

/// An opaque saved plugin or device state of one format.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateChunk {
    /// Format tag such as `clap`, `vst3` or `builtin`.
    pub format: String,
    /// Base64 text of the state bytes, written and read by the device host.
    pub data: String,
}

/// An instrument, effect or hosted plugin instance.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    /// Id.
    pub id: DeviceId,
    /// What it is: `builtin.<name>` or `<format>:<identifier>`.
    pub kind: String,
    /// Display name.
    pub name: String,
    /// Processing enabled (false is bypass).
    pub enabled: bool,
    /// Parameter values by parameter id; unset parameters hold their defaults.
    #[serde(default)]
    pub params: BTreeMap<u32, f64>,
    /// Saved state, per format.
    #[serde(default)]
    pub state: Vec<StateChunk>,
    /// Sidechain source strip, if the device takes one.
    #[serde(default)]
    pub sidechain: Option<MixerRef>,
}

/// Which strip parameter an automation lane drives.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StripParam {
    /// Fader gain (dB).
    Gain,
    /// Pan.
    Pan,
}

/// What an automation lane drives.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationTarget {
    /// A device parameter.
    Device {
        /// The device.
        device: DeviceId,
        /// Parameter id.
        param: u32,
    },
    /// A strip parameter.
    Strip {
        /// The strip.
        strip: MixerRef,
        /// The parameter.
        param: StripParam,
    },
}

/// A breakpoint of an automation lane.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AutomationPoint {
    /// Position in beats.
    pub beat: f64,
    /// Value in the target's units.
    pub value: f64,
    /// Curve toward the next point: `-1.0` to `1.0`, `0.0` linear.
    pub curve: f64,
}

/// An automation lane: a breakpoint envelope over the timeline.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomationLane {
    /// Id.
    pub id: LaneId,
    /// What it drives.
    pub target: AutomationTarget,
    /// Breakpoints ordered by position.
    pub points: Vec<AutomationPoint>,
    /// Whether the lane is applied.
    pub enabled: bool,
}

/// A named position on the timeline.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    /// Id.
    pub id: MarkerId,
    /// Display name.
    pub name: String,
    /// Position in beats.
    pub beat: f64,
    /// Color as `0xRRGGBB`.
    pub color: u32,
}

/// A beat range.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BeatRange {
    /// Start, in beats.
    pub start: f64,
    /// End, in beats; after `start`.
    pub end: f64,
}

/// Project-wide settings.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSettings {
    /// Preferred sample rate in hertz.
    pub sample_rate: u32,
    /// Loop range, if set.
    #[serde(default)]
    pub loop_range: Option<BeatRange>,
    /// Bars of metronome count-in before recording, 0 to 16.
    #[serde(default)]
    pub count_in_bars: u8,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            loop_range: None,
            count_in_bars: 0,
        }
    }
}

macro_rules! keyed_impl {
    ($($ty:ty => $id:ty),* $(,)?) => {$(
        impl Keyed for $ty {
            type Id = $id;
            fn id(&self) -> $id {
                self.id
            }
        }
    )*};
}

keyed_impl! {
    Clip => ClipId,
    Pattern => PatternId,
    MediaItem => MediaId,
    Device => DeviceId,
    AutomationLane => LaneId,
    Marker => MarkerId,
}

/// Every id up to the limit has been handed out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdExhausted;

impl std::fmt::Display for IdExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the project has run out of ids")
    }
}

impl std::error::Error for IdExhausted {}

/// The whole project.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// Display name.
    pub name: String,
    /// Project-wide settings.
    #[serde(default)]
    pub settings: ProjectSettings,
    /// Tempo over the timeline.
    #[cfg_attr(feature = "ts", ts(as = "Vec<crate::time_serde::PointRepr>"))]
    pub tempo_map: TempoMap,
    /// Time signatures over the timeline.
    #[cfg_attr(feature = "ts", ts(as = "Vec<crate::time_serde::ChangeRepr>"))]
    pub time_signature_map: TimeSignatureMap,
    /// Tracks in display order.
    #[serde(default)]
    pub tracks: Vec<Track>,
    /// Buses in display order.
    #[serde(default)]
    pub buses: Vec<Bus>,
    /// The master strip.
    #[serde(default)]
    pub master: Strip,
    /// Clips.
    #[serde(default, with = "keyed")]
    #[cfg_attr(feature = "ts", ts(as = "Vec<Clip>"))]
    pub clips: BTreeMap<ClipId, Clip>,
    /// Patterns.
    #[serde(default, with = "keyed")]
    #[cfg_attr(feature = "ts", ts(as = "Vec<Pattern>"))]
    pub patterns: BTreeMap<PatternId, Pattern>,
    /// The media pool.
    #[serde(default, with = "keyed")]
    #[cfg_attr(feature = "ts", ts(as = "Vec<MediaItem>"))]
    pub media_pool: BTreeMap<MediaId, MediaItem>,
    /// Devices; each sits in exactly one strip's insert chain or is a track's instrument.
    #[serde(default, with = "keyed")]
    #[cfg_attr(feature = "ts", ts(as = "Vec<Device>"))]
    pub devices: BTreeMap<DeviceId, Device>,
    /// Automation lanes.
    #[serde(default, with = "keyed")]
    #[cfg_attr(feature = "ts", ts(as = "Vec<AutomationLane>"))]
    pub automation: BTreeMap<LaneId, AutomationLane>,
    /// Markers.
    #[serde(default, with = "keyed")]
    #[cfg_attr(feature = "ts", ts(as = "Vec<Marker>"))]
    pub markers: BTreeMap<MarkerId, Marker>,
    /// Next id [`alloc_id`](Self::alloc_id) hands out; above every id in use.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_id: u64,
}

impl Default for Project {
    fn default() -> Self {
        Self::new("Untitled")
    }
}

impl Project {
    /// An empty project at 120 bpm in 4/4.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            settings: ProjectSettings::default(),
            tempo_map: TempoMap::default(),
            time_signature_map: TimeSignatureMap::default(),
            tracks: Vec::new(),
            buses: Vec::new(),
            master: Strip::default(),
            clips: BTreeMap::new(),
            patterns: BTreeMap::new(),
            media_pool: BTreeMap::new(),
            devices: BTreeMap::new(),
            automation: BTreeMap::new(),
            markers: BTreeMap::new(),
            next_id: 1,
        }
    }

    /// Hands out a fresh id (never reused, unique across entity kinds). Not undoable: undo
    /// never rewinds the counter, so an id stays unique for the life of the project.
    ///
    /// Fails with [`IdExhausted`] once ids reach [`MAX_ID`](crate::validate::MAX_ID); the counter
    /// never wraps or re-issues an id.
    pub fn alloc_id<I: From<u64>>(&mut self) -> Result<I, IdExhausted> {
        let id = self.next_id;
        if id > MAX_ID {
            return Err(IdExhausted);
        }
        self.next_id = id + 1;
        Ok(I::from(id))
    }

    /// The track with `id`.
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// The bus with `id`.
    pub fn bus(&self, id: BusId) -> Option<&Bus> {
        self.buses.iter().find(|b| b.id == id)
    }

    /// The strip `target` names.
    pub fn strip(&self, target: MixerRef) -> Option<&Strip> {
        match target {
            MixerRef::Track(id) => self.track(id).map(|t| &t.strip),
            MixerRef::Bus(id) => self.bus(id).map(|b| &b.strip),
            MixerRef::Master => Some(&self.master),
        }
    }

    pub(crate) fn strip_mut(&mut self, target: MixerRef) -> Option<&mut Strip> {
        match target {
            MixerRef::Track(id) => self
                .tracks
                .iter_mut()
                .find(|t| t.id == id)
                .map(|t| &mut t.strip),
            MixerRef::Bus(id) => self
                .buses
                .iter_mut()
                .find(|b| b.id == id)
                .map(|b| &mut b.strip),
            MixerRef::Master => Some(&mut self.master),
        }
    }

    /// True when `id` is the id of any entity.
    pub fn id_in_use(&self, id: u64) -> bool {
        self.tracks.iter().any(|t| t.id.0 == id)
            || self.buses.iter().any(|b| b.id.0 == id)
            || self.clips.contains_key(&ClipId(id))
            || self.patterns.contains_key(&PatternId(id))
            || self.media_pool.contains_key(&MediaId(id))
            || self.devices.contains_key(&DeviceId(id))
            || self.automation.contains_key(&LaneId(id))
            || self.markers.contains_key(&MarkerId(id))
    }

    /// Equality of everything except the id counter, which only ever grows (used to check that
    /// undo restores the model).
    pub fn same_content(&self, other: &Project) -> bool {
        let mut a = self.clone();
        let mut b = other.clone();
        a.next_id = 0;
        b.next_id = 0;
        a == b
    }
}
