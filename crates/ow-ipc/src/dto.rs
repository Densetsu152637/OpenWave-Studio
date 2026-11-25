//! The data transfer objects: what the UI sends ([`UiCommand`]) and what the control thread
//! answers with ([`Snapshot`], [`ModelPatch`]).
//!
//! Every type derives `ts_rs::TS`; the generated TypeScript lives in `src/lib/ipc/generated/`.
//! Project entities are the `ow-model` types themselves (re-exported from the crate root), so the
//! UI sees exactly the `project.json` shape: keyed collections (clips, patterns, media, devices,
//! automation, markers) are lists.

use ow_model::ids::{BusId, ClipId, DeviceId, LaneId, MarkerId, MediaId, PatternId, TrackId};
use ow_model::{
    AutomationLane, Bus, Clip, Device, EditCommand, Marker, MediaItem, Pattern, Project,
    ProjectSettings, Strip, Track,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A request from the UI to the control thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, strum::VariantNames)]
#[serde(tag = "type", rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum UiCommand {
    /// Applies an edit (one undo step, or part of the open transaction).
    Edit {
        /// The edit.
        edit: EditCommand,
    },
    /// Reverts the latest undo step.
    Undo,
    /// Reapplies the latest undone step.
    Redo,
    /// Opens a transaction (a drag or gesture): its edits undo as one step named `label`.
    Begin {
        /// Undo-menu label.
        label: String,
    },
    /// Closes the innermost transaction.
    End,
    /// Abandons the open transaction, reverting its edits.
    Cancel,
    /// Reserves `count` fresh entity ids (at most [`MAX_ALLOC_IDS`]) for the UI to use in the
    /// entities its next edits create. Ids are unique across entity kinds and never reused.
    AllocIds {
        /// How many ids.
        count: u32,
    },
}

/// Most ids one [`UiCommand::AllocIds`] hands out.
pub const MAX_ALLOC_IDS: u32 = 1024;

/// The undo and redo state the UI shows in menus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct HistoryState {
    /// An undo step exists.
    pub can_undo: bool,
    /// A redo step exists.
    pub can_redo: bool,
    /// Label of the step undo would revert.
    pub undo_label: Option<String>,
    /// Label of the step redo would reapply.
    pub redo_label: Option<String>,
    /// A transaction is open (undo and redo are refused meanwhile).
    pub in_transaction: bool,
    /// The project differs from what was last marked saved.
    pub dirty: bool,
    /// Why the audio engine is not playing the current project (its plan could not be swapped
    /// in); null while it is. The engine keeps the previous plan and the app retries.
    pub engine_error: Option<String>,
    /// Media items the engine could not load when it last compiled the project (the file is
    /// missing or unreadable); their clips play silence. A warning, not an error: the plan is
    /// live. Ids in ascending order.
    #[ts(type = "Array<number>")]
    pub missing_media: Vec<u64>,
    /// Audio tracks armed for recording, ascending ids. Transient session state: not saved, not
    /// part of the model, no undo and no dirty flag; cleared when another project is loaded.
    #[ts(type = "Array<number>")]
    pub armed_tracks: Vec<u64>,
}

/// One change to the mirrored project. Applying a patch's operations in order to the mirror
/// reproduces the control thread's project; the UI never applies anything else.
///
/// `Upsert*` replaces the entity with the same id, or adds it. `Order*` lists the complete
/// display order of tracks or buses and is sent whenever the order is not simply "existing
/// entries in their old order, then new ones appended".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOp {
    /// Replaces the whole project (a project was opened or created). The patch also carries a
    /// `SetProjectPath` for the new project and no other operation.
    Reset {
        /// The new project.
        project: Box<Project>,
    },
    /// Where the project is saved (`null` while it was never saved); not part of the project.
    SetProjectPath {
        /// The `.owave` bundle path.
        project_path: Option<String>,
    },
    /// Project name.
    SetName {
        /// New name.
        name: String,
    },
    /// Project settings.
    SetSettings {
        /// New settings.
        settings: ProjectSettings,
    },
    /// Tempo map.
    SetTempoMap {
        /// New map.
        #[ts(as = "Vec<ow_model::TempoPointJson>")]
        tempo_map: ow_model::time::TempoMap,
    },
    /// Time-signature map.
    SetTimeSignatureMap {
        /// New map.
        #[ts(as = "Vec<ow_model::SignatureChangeJson>")]
        map: ow_model::time::TimeSignatureMap,
    },
    /// The master strip.
    SetMaster {
        /// New master strip.
        master: Strip,
    },
    /// The id counter.
    SetNextId {
        /// New counter.
        #[ts(type = "number")]
        next_id: u64,
    },
    /// Adds or replaces a track.
    UpsertTrack {
        /// The track.
        track: Track,
    },
    /// Removes a track.
    RemoveTrack {
        /// The track.
        id: TrackId,
    },
    /// Track display order.
    OrderTracks {
        /// Every track id, in order.
        ids: Vec<TrackId>,
    },
    /// Adds or replaces a bus.
    UpsertBus {
        /// The bus.
        bus: Bus,
    },
    /// Removes a bus.
    RemoveBus {
        /// The bus.
        id: BusId,
    },
    /// Bus display order.
    OrderBuses {
        /// Every bus id, in order.
        ids: Vec<BusId>,
    },
    /// Adds or replaces a clip.
    UpsertClip {
        /// The clip.
        clip: Clip,
    },
    /// Removes a clip.
    RemoveClip {
        /// The clip.
        id: ClipId,
    },
    /// Adds or replaces a pattern.
    UpsertPattern {
        /// The pattern.
        pattern: Pattern,
    },
    /// Removes a pattern.
    RemovePattern {
        /// The pattern.
        id: PatternId,
    },
    /// Adds or replaces a media pool item.
    UpsertMedia {
        /// The item.
        item: MediaItem,
    },
    /// Removes a media pool item.
    RemoveMedia {
        /// The item.
        id: MediaId,
    },
    /// Adds or replaces a device.
    UpsertDevice {
        /// The device.
        device: Device,
    },
    /// Removes a device.
    RemoveDevice {
        /// The device.
        id: DeviceId,
    },
    /// Adds or replaces an automation lane.
    UpsertLane {
        /// The lane.
        lane: AutomationLane,
    },
    /// Removes an automation lane.
    RemoveLane {
        /// The lane.
        id: LaneId,
    },
    /// Adds or replaces a marker.
    UpsertMarker {
        /// The marker.
        marker: Marker,
    },
    /// Removes a marker.
    RemoveMarker {
        /// The marker.
        id: MarkerId,
    },
}

/// An ordered change set. `seq` is 1 for the first patch after a snapshot with `seq` 0 and grows
/// by exactly one per patch; a mirror that receives anything else has missed a patch and
/// resynchronizes from a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ModelPatch {
    /// Sequence number, strictly increasing by one.
    #[ts(type = "number")]
    pub seq: u64,
    /// The changes, to apply in order.
    pub ops: Vec<PatchOp>,
    /// Undo and redo state after the change.
    pub history: HistoryState,
}

/// The whole project at sequence `seq`: patches numbered `seq + 1` and up apply on top of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct Snapshot {
    /// Sequence number of the last patch this snapshot includes.
    #[ts(type = "number")]
    pub seq: u64,
    /// The project.
    pub project: Project,
    /// Undo and redo state.
    pub history: HistoryState,
    /// The `.owave` bundle the project is saved in; `null` while it is unsaved.
    pub project_path: Option<String>,
}

/// What `media_import` answers: the media item the file became (an existing one when the same
/// content was imported before). The file is stored at the project rate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct MediaImportReply {
    /// Id of the media pool item.
    #[ts(type = "number")]
    pub media: u64,
    /// Length in frames at the project rate.
    #[ts(type = "number")]
    pub frames: u64,
    /// Sample rate of the stored file (the project rate).
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
}

/// What `midi_import` answers: what the file became in the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct MidiImportReply {
    /// Tracks the import created (empty when it went onto an existing track).
    #[ts(type = "Array<number>")]
    pub tracks: Vec<u64>,
    /// The clips added, one per track of the file that holds notes or control changes.
    #[ts(type = "Array<number>")]
    pub clips: Vec<u64>,
    /// Whether the file's tempo changes replaced the project's tempo map.
    pub tempo_applied: bool,
}

/// What `midi_export` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct MidiExportReply {
    /// The file written.
    pub path: String,
    /// Tracks in the file (not counting its tempo track).
    pub tracks: u32,
    /// Notes in the file.
    pub notes: u32,
}

/// What `midi_edit` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct MidiEditReply {
    /// The clip that was edited.
    #[ts(type = "number")]
    pub clip: u64,
    /// Notes in the clip before the edit.
    pub notes_before: u32,
    /// Notes in the clip after it (fewer when a transpose moved notes out of range).
    pub notes_after: u32,
}

/// What a command answers: the sequence number after it (unchanged when the command did not
/// change anything) and, for `alloc_ids`, the reserved ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CommandReply {
    /// Sequence number of the latest patch.
    #[ts(type = "number")]
    pub seq: u64,
    /// Ids reserved by `alloc_ids` (empty for every other command).
    #[ts(type = "Array<number>")]
    pub ids: Vec<u64>,
    /// Set when the edit was applied to the project but the audio engine could not take the new
    /// plan (it keeps playing the previous one and the app retries on the next command).
    pub engine_error: Option<String>,
}
