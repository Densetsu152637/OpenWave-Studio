//! Edit commands: every change to a [`Project`] is an [`EditCommand`], and applying one returns
//! its inverse, which [`Editor`](crate::Editor) pushes on the undo stack.
//!
//! Applying a command is atomic: on error the project is exactly as before. Commands check every
//! invariant [`Project::validate`] enforces for what they touch, so a hostile or buggy sender
//! (a UI message, a pasted clip) cannot produce a project that fails validation.
//!
//! Removing something other entities still point at is refused ([`EditError::InUse`]) instead
//! of cascading: the sender reroutes or deletes the dependents first, inside one transaction.
//! What an entity owns (a track's clips, devices and automation lanes) goes with it, and the
//! inverse restores all of it.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::{BusId, ClipId, DeviceId, LaneId, MarkerId, MediaId, PatternId, TrackId};
use crate::model::{
    AutomationLane, AutomationTarget, AuxSend, Bus, Clip, ClipContent, Device, Marker, MediaItem,
    MixerRef, Pattern, Project, ProjectSettings, StateChunk, Strip, StripSettings, Track,
};
use crate::time::{TempoMap, TimeSignatureMap};
use crate::validate::{ValidationError, check_marker, check_media, check_pattern, check_settings};

/// Why a command was refused. The project is unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum EditError {
    /// A value or reference breaks a model invariant.
    Invalid(ValidationError),
    /// No entity of that kind has that id.
    NotFound {
        /// Entity kind.
        kind: &'static str,
        /// The id.
        id: u64,
    },
    /// The id is already used by an entity.
    IdTaken(u64),
    /// Other entities still refer to it.
    InUse {
        /// Entity kind.
        kind: &'static str,
        /// The id.
        id: u64,
    },
    /// An index is past the end of its list.
    IndexOutOfRange,
    /// The command does not apply to that target (for example renaming the master).
    WrongTarget(&'static str),
    /// The instrument slot already holds a device.
    SlotOccupied,
    /// The change would make buses route into each other in a cycle.
    RoutingCycle,
    /// `end` or `cancel` without `begin`.
    NoTransaction,
    /// Undo or redo while a transaction is open.
    TransactionOpen,
    /// The undo stack is empty.
    NothingToUndo,
    /// The redo stack is empty.
    NothingToRedo,
    /// An inverse failed to apply, which is a bug; the history was discarded.
    Corrupt(String),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(e) => write!(f, "invalid edit: {e}"),
            Self::NotFound { kind, id } => write!(f, "no {kind} with id {id}"),
            Self::IdTaken(id) => write!(f, "id {id} is already in use"),
            Self::InUse { kind, id } => write!(f, "{kind} {id} is still in use"),
            Self::IndexOutOfRange => f.write_str("index out of range"),
            Self::WrongTarget(what) => write!(f, "not applicable: {what}"),
            Self::SlotOccupied => f.write_str("the instrument slot is occupied"),
            Self::RoutingCycle => f.write_str("the routing would contain a cycle"),
            Self::NoTransaction => f.write_str("no transaction is open"),
            Self::TransactionOpen => f.write_str("a transaction is open"),
            Self::NothingToUndo => f.write_str("nothing to undo"),
            Self::NothingToRedo => f.write_str("nothing to redo"),
            Self::Corrupt(why) => write!(f, "history corrupt: {why}"),
        }
    }
}

impl std::error::Error for EditError {}

impl From<ValidationError> for EditError {
    fn from(error: ValidationError) -> Self {
        Self::Invalid(error)
    }
}

/// A track or bus with everything it owns: its clips, the devices in its slots and the
/// automation lanes that target it or its devices. The unit of insert and remove.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Owned<T> {
    /// The track or bus.
    pub item: T,
    /// Clips on the track (empty for a bus).
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// Devices in the strip's insert chain and the track's instrument.
    #[serde(default)]
    pub devices: Vec<Device>,
    /// Automation lanes targeting the strip or one of the devices.
    #[serde(default)]
    pub lanes: Vec<AutomationLane>,
}

impl<T> Owned<T> {
    /// A bundle holding only `item`.
    pub fn new(item: T) -> Self {
        Self {
            item,
            clips: Vec::new(),
            devices: Vec::new(),
            lanes: Vec::new(),
        }
    }
}

/// A device with the automation lanes that target it.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceBundle {
    /// The device.
    pub device: Device,
    /// Lanes targeting it.
    #[serde(default)]
    pub lanes: Vec<AutomationLane>,
}

/// Where a device sits.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceSlot {
    /// The instrument of a MIDI track.
    Instrument {
        /// The track.
        track: TrackId,
    },
    /// Position `index` in a strip's insert chain.
    Insert {
        /// The strip.
        strip: MixerRef,
        /// Position in the chain.
        index: usize,
    },
}

/// A change to a [`Project`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, strum::VariantNames)]
#[serde(tag = "command", rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum EditCommand {
    /// Renames the project.
    SetName {
        /// New name.
        name: String,
    },
    /// Replaces the project settings.
    SetSettings {
        /// New settings.
        settings: ProjectSettings,
    },
    /// Replaces the tempo map.
    SetTempoMap {
        /// New map.
        #[cfg_attr(feature = "ts", ts(as = "Vec<crate::time_serde::PointRepr>"))]
        #[cfg_attr(
            feature = "schema",
            schemars(with = "Vec<crate::time_serde::PointRepr>")
        )]
        tempo_map: TempoMap,
    },
    /// Replaces the time-signature map.
    SetTimeSignatureMap {
        /// New map.
        #[cfg_attr(feature = "ts", ts(as = "Vec<crate::time_serde::ChangeRepr>"))]
        #[cfg_attr(
            feature = "schema",
            schemars(with = "Vec<crate::time_serde::ChangeRepr>")
        )]
        map: TimeSignatureMap,
    },
    /// Inserts a track (with what it owns) at `index`.
    InsertTrack {
        /// Position in the track list.
        index: usize,
        /// The track and its contents.
        bundle: Box<Owned<Track>>,
    },
    /// Removes a track and everything it owns.
    RemoveTrack {
        /// The track.
        id: TrackId,
    },
    /// Moves a track to `to` (its index after the move).
    MoveTrack {
        /// The track.
        id: TrackId,
        /// New index.
        to: usize,
    },
    /// Inserts a bus (with its devices and lanes) at `index`.
    InsertBus {
        /// Position in the bus list.
        index: usize,
        /// The bus and its contents.
        bundle: Box<Owned<Bus>>,
    },
    /// Removes a bus and everything it owns; refused while anything routes to it.
    RemoveBus {
        /// The bus.
        id: BusId,
    },
    /// Moves a bus to `to` (its index after the move).
    MoveBus {
        /// The bus.
        id: BusId,
        /// New index.
        to: usize,
    },
    /// Renames and recolors a track or bus.
    SetInfo {
        /// A track or bus (not the master).
        target: MixerRef,
        /// New name.
        name: String,
        /// New color.
        color: u32,
    },
    /// Replaces a strip's fader, pan, mute, solo and output.
    SetStripSettings {
        /// The strip.
        target: MixerRef,
        /// New settings.
        settings: StripSettings,
    },
    /// Replaces a strip's sends.
    SetSends {
        /// The strip.
        target: MixerRef,
        /// New sends.
        sends: Vec<AuxSend>,
    },
    /// Puts a device in a slot.
    InsertDevice {
        /// Where.
        slot: DeviceSlot,
        /// The device and its lanes.
        bundle: Box<DeviceBundle>,
    },
    /// Removes a device and the lanes that target it.
    RemoveDevice {
        /// The device.
        id: DeviceId,
    },
    /// Sets (or with `None` clears) a device parameter value.
    SetDeviceParam {
        /// The device.
        device: DeviceId,
        /// Parameter id.
        param: u32,
        /// New value.
        value: Option<f64>,
    },
    /// Replaces a device's saved state.
    SetDeviceState {
        /// The device.
        device: DeviceId,
        /// New state.
        state: Vec<StateChunk>,
    },
    /// Enables or bypasses a device.
    SetDeviceEnabled {
        /// The device.
        device: DeviceId,
        /// Enabled.
        enabled: bool,
    },
    /// Sets a device's sidechain source.
    SetDeviceSidechain {
        /// The device.
        device: DeviceId,
        /// The source strip.
        source: Option<MixerRef>,
    },
    /// Adds a clip.
    AddClip {
        /// The clip.
        clip: Box<Clip>,
    },
    /// Removes a clip.
    RemoveClip {
        /// The clip.
        id: ClipId,
    },
    /// Replaces a clip (same id): move, trim, retarget, edit its content.
    ReplaceClip {
        /// The new clip.
        clip: Box<Clip>,
    },
    /// Adds a pattern.
    AddPattern {
        /// The pattern.
        pattern: Pattern,
    },
    /// Removes a pattern; refused while an instance uses it.
    RemovePattern {
        /// The pattern.
        id: PatternId,
    },
    /// Replaces a pattern (same id); every instance follows.
    ReplacePattern {
        /// The new pattern.
        pattern: Pattern,
    },
    /// Gives a pattern instance its own copy of the pattern (under the new id `pattern`).
    MakeUnique {
        /// The instance clip.
        clip: ClipId,
        /// Id for the copy.
        pattern: PatternId,
    },
    /// Adds a media pool item.
    AddMedia {
        /// The item.
        item: MediaItem,
    },
    /// Removes a media item; refused while a clip uses it.
    RemoveMedia {
        /// The item.
        id: MediaId,
    },
    /// Replaces a media item (same id), for example when relinking.
    ReplaceMedia {
        /// The new item.
        item: MediaItem,
    },
    /// Adds an automation lane.
    AddLane {
        /// The lane.
        lane: AutomationLane,
    },
    /// Removes an automation lane.
    RemoveLane {
        /// The lane.
        id: LaneId,
    },
    /// Replaces an automation lane (same id).
    ReplaceLane {
        /// The new lane.
        lane: AutomationLane,
    },
    /// Adds a marker.
    AddMarker {
        /// The marker.
        marker: Marker,
    },
    /// Removes a marker.
    RemoveMarker {
        /// The marker.
        id: MarkerId,
    },
    /// Replaces a marker (same id).
    ReplaceMarker {
        /// The new marker.
        marker: Marker,
    },
    /// Applies the commands in order, all or none.
    Batch {
        /// The commands.
        commands: Vec<EditCommand>,
    },
}

impl EditCommand {
    /// A short human-readable label for undo menus.
    pub fn label(&self) -> &'static str {
        match self {
            Self::SetName { .. } => "Rename project",
            Self::SetSettings { .. } => "Change project settings",
            Self::SetTempoMap { .. } => "Change tempo",
            Self::SetTimeSignatureMap { .. } => "Change time signature",
            Self::InsertTrack { .. } => "Add track",
            Self::RemoveTrack { .. } => "Delete track",
            Self::MoveTrack { .. } => "Move track",
            Self::InsertBus { .. } => "Add bus",
            Self::RemoveBus { .. } => "Delete bus",
            Self::MoveBus { .. } => "Move bus",
            Self::SetInfo { .. } => "Rename",
            Self::SetStripSettings { .. } => "Change mixer",
            Self::SetSends { .. } => "Change sends",
            Self::InsertDevice { .. } => "Add device",
            Self::RemoveDevice { .. } => "Remove device",
            Self::SetDeviceParam { .. } => "Change parameter",
            Self::SetDeviceState { .. } => "Change device state",
            Self::SetDeviceEnabled { .. } => "Toggle device",
            Self::SetDeviceSidechain { .. } => "Change sidechain",
            Self::AddClip { .. } => "Add clip",
            Self::RemoveClip { .. } => "Delete clip",
            Self::ReplaceClip { .. } => "Edit clip",
            Self::AddPattern { .. } => "Add pattern",
            Self::RemovePattern { .. } => "Delete pattern",
            Self::ReplacePattern { .. } => "Edit pattern",
            Self::MakeUnique { .. } => "Make pattern unique",
            Self::AddMedia { .. } => "Add media",
            Self::RemoveMedia { .. } => "Remove media",
            Self::ReplaceMedia { .. } => "Edit media",
            Self::AddLane { .. } => "Add automation",
            Self::RemoveLane { .. } => "Delete automation",
            Self::ReplaceLane { .. } => "Edit automation",
            Self::AddMarker { .. } => "Add marker",
            Self::RemoveMarker { .. } => "Delete marker",
            Self::ReplaceMarker { .. } => "Edit marker",
            Self::Batch { .. } => "Edit",
        }
    }
}

fn not_found(kind: &'static str, id: u64) -> EditError {
    EditError::NotFound { kind, id }
}

fn in_use(kind: &'static str, id: u64) -> EditError {
    EditError::InUse { kind, id }
}

impl Project {
    /// Applies `command` and returns the command that undoes it. Atomic: on error nothing
    /// changed. Applying the returned inverse returns the original command (or an equivalent).
    pub fn apply(&mut self, command: EditCommand) -> Result<EditCommand, EditError> {
        use EditCommand as C;
        match command {
            C::SetName { name } => {
                crate::validate::check_name("name", &name)?;
                let old = std::mem::replace(&mut self.name, name);
                Ok(C::SetName { name: old })
            }
            C::SetSettings { settings } => {
                check_settings(&settings)?;
                let old = std::mem::replace(&mut self.settings, settings);
                Ok(C::SetSettings { settings: old })
            }
            C::SetTempoMap { tempo_map } => {
                let old = std::mem::replace(&mut self.tempo_map, tempo_map);
                Ok(C::SetTempoMap { tempo_map: old })
            }
            C::SetTimeSignatureMap { map } => {
                let old = std::mem::replace(&mut self.time_signature_map, map);
                Ok(C::SetTimeSignatureMap { map: old })
            }
            C::InsertTrack { index, bundle } => {
                let id = bundle.item.id;
                self.insert_track(index, *bundle)?;
                Ok(C::RemoveTrack { id })
            }
            C::RemoveTrack { id } => {
                let (index, bundle) = self.remove_track(id)?;
                Ok(C::InsertTrack {
                    index,
                    bundle: Box::new(bundle),
                })
            }
            C::MoveTrack { id, to } => {
                let from = self
                    .tracks
                    .iter()
                    .position(|t| t.id == id)
                    .ok_or_else(|| not_found("track", id.0))?;
                if to >= self.tracks.len() {
                    return Err(EditError::IndexOutOfRange);
                }
                let track = self.tracks.remove(from);
                self.tracks.insert(to, track);
                Ok(C::MoveTrack { id, to: from })
            }
            C::InsertBus { index, bundle } => {
                let id = bundle.item.id;
                self.insert_bus(index, *bundle)?;
                Ok(C::RemoveBus { id })
            }
            C::RemoveBus { id } => {
                let (index, bundle) = self.remove_bus(id)?;
                Ok(C::InsertBus {
                    index,
                    bundle: Box::new(bundle),
                })
            }
            C::MoveBus { id, to } => {
                let from = self
                    .buses
                    .iter()
                    .position(|b| b.id == id)
                    .ok_or_else(|| not_found("bus", id.0))?;
                if to >= self.buses.len() {
                    return Err(EditError::IndexOutOfRange);
                }
                let bus = self.buses.remove(from);
                self.buses.insert(to, bus);
                Ok(C::MoveBus { id, to: from })
            }
            C::SetInfo {
                target,
                name,
                color,
            } => {
                crate::validate::check_name("name", &name)?;
                crate::validate::check_color("color", color)?;
                match target {
                    MixerRef::Track(id) => {
                        let track = self
                            .tracks
                            .iter_mut()
                            .find(|t| t.id == id)
                            .ok_or_else(|| not_found("track", id.0))?;
                        let old_name = std::mem::replace(&mut track.name, name);
                        let old_color = std::mem::replace(&mut track.color, color);
                        Ok(C::SetInfo {
                            target,
                            name: old_name,
                            color: old_color,
                        })
                    }
                    MixerRef::Bus(id) => {
                        let bus = self
                            .buses
                            .iter_mut()
                            .find(|b| b.id == id)
                            .ok_or_else(|| not_found("bus", id.0))?;
                        let old_name = std::mem::replace(&mut bus.name, name);
                        let old_color = std::mem::replace(&mut bus.color, color);
                        Ok(C::SetInfo {
                            target,
                            name: old_name,
                            color: old_color,
                        })
                    }
                    MixerRef::Master => Err(EditError::WrongTarget("the master has no name")),
                }
            }
            C::SetStripSettings { target, settings } => {
                let old = self.replace_strip(target, |strip| strip.settings = settings)?;
                Ok(C::SetStripSettings {
                    target,
                    settings: old.settings,
                })
            }
            C::SetSends { target, sends } => {
                let old = self.replace_strip(target, |strip| strip.sends = sends)?;
                Ok(C::SetSends {
                    target,
                    sends: old.sends,
                })
            }
            C::InsertDevice { slot, bundle } => {
                let id = bundle.device.id;
                self.insert_device(slot, *bundle)?;
                Ok(C::RemoveDevice { id })
            }
            C::RemoveDevice { id } => {
                let (slot, bundle) = self.remove_device(id)?;
                Ok(C::InsertDevice {
                    slot,
                    bundle: Box::new(bundle),
                })
            }
            C::SetDeviceParam {
                device,
                param,
                value,
            } => {
                if let Some(v) = value
                    && !v.is_finite()
                {
                    return Err(EditError::Invalid(ValidationError {
                        path: "value".into(),
                        message: "must be finite".into(),
                    }));
                }
                let d = self
                    .devices
                    .get_mut(&device)
                    .ok_or_else(|| not_found("device", device.0))?;
                let old = match value {
                    Some(v) => d.params.insert(param, v),
                    None => d.params.remove(&param),
                };
                Ok(C::SetDeviceParam {
                    device,
                    param,
                    value: old,
                })
            }
            C::SetDeviceState { device, state } => {
                let mut candidate = self
                    .devices
                    .get(&device)
                    .ok_or_else(|| not_found("device", device.0))?
                    .clone();
                candidate.state = state;
                self.check_device(&candidate)?;
                let d = self.devices.get_mut(&device).expect("checked above");
                let old = std::mem::replace(&mut d.state, candidate.state);
                Ok(C::SetDeviceState { device, state: old })
            }
            C::SetDeviceEnabled { device, enabled } => {
                let d = self
                    .devices
                    .get_mut(&device)
                    .ok_or_else(|| not_found("device", device.0))?;
                let old = std::mem::replace(&mut d.enabled, enabled);
                Ok(C::SetDeviceEnabled {
                    device,
                    enabled: old,
                })
            }
            C::SetDeviceSidechain { device, source } => {
                if !self.devices.contains_key(&device) {
                    return Err(not_found("device", device.0));
                }
                if let Some(s) = source
                    && self.strip(s).is_none()
                {
                    return Err(EditError::Invalid(ValidationError {
                        path: "source".into(),
                        message: "unknown strip".into(),
                    }));
                }
                let d = self.devices.get_mut(&device).expect("checked above");
                let old = std::mem::replace(&mut d.sidechain, source);
                if !self.routing_is_acyclic() {
                    if let Some(d) = self.devices.get_mut(&device) {
                        d.sidechain = old;
                    }
                    return Err(EditError::RoutingCycle);
                }
                Ok(C::SetDeviceSidechain {
                    device,
                    source: old,
                })
            }
            C::AddClip { clip } => {
                let id = clip.id;
                self.check_new_id(id.0)?;
                self.check_clip(&clip)?;
                self.clips.insert(id, *clip);
                self.bump_next_id(id.0);
                Ok(C::RemoveClip { id })
            }
            C::RemoveClip { id } => {
                let clip = self
                    .clips
                    .remove(&id)
                    .ok_or_else(|| not_found("clip", id.0))?;
                Ok(C::AddClip {
                    clip: Box::new(clip),
                })
            }
            C::ReplaceClip { clip } => {
                let id = clip.id;
                if !self.clips.contains_key(&id) {
                    return Err(not_found("clip", id.0));
                }
                self.check_clip(&clip)?;
                let old = self.clips.insert(id, *clip).expect("checked above");
                Ok(C::ReplaceClip {
                    clip: Box::new(old),
                })
            }
            C::AddPattern { pattern } => {
                let id = pattern.id;
                self.check_new_id(id.0)?;
                check_pattern(&pattern)?;
                self.patterns.insert(id, pattern);
                self.bump_next_id(id.0);
                Ok(C::RemovePattern { id })
            }
            C::RemovePattern { id } => {
                if !self.patterns.contains_key(&id) {
                    return Err(not_found("pattern", id.0));
                }
                if self
                    .clips
                    .values()
                    .any(|c| matches!(c.content, ClipContent::Pattern { pattern } if pattern == id))
                {
                    return Err(in_use("pattern", id.0));
                }
                let pattern = self.patterns.remove(&id).expect("checked above");
                Ok(C::AddPattern { pattern })
            }
            C::ReplacePattern { pattern } => {
                let id = pattern.id;
                if !self.patterns.contains_key(&id) {
                    return Err(not_found("pattern", id.0));
                }
                check_pattern(&pattern)?;
                let old = self.patterns.insert(id, pattern).expect("checked above");
                Ok(C::ReplacePattern { pattern: old })
            }
            C::MakeUnique { clip, pattern } => {
                let existing = self
                    .clips
                    .get(&clip)
                    .ok_or_else(|| not_found("clip", clip.0))?;
                let ClipContent::Pattern { pattern: source } = existing.content else {
                    return Err(EditError::WrongTarget("the clip is not a pattern instance"));
                };
                self.check_new_id(pattern.0)?;
                let mut copy = self
                    .patterns
                    .get(&source)
                    .ok_or_else(|| not_found("pattern", source.0))?
                    .clone();
                copy.id = pattern;
                let mut changed = existing.clone();
                let old_clip = existing.clone();
                changed.content = ClipContent::Pattern { pattern };
                self.patterns.insert(pattern, copy);
                self.bump_next_id(pattern.0);
                self.clips.insert(clip, changed);
                Ok(C::Batch {
                    commands: vec![
                        C::ReplaceClip {
                            clip: Box::new(old_clip),
                        },
                        C::RemovePattern { id: pattern },
                    ],
                })
            }
            C::AddMedia { item } => {
                let id = item.id;
                self.check_new_id(id.0)?;
                check_media(&item)?;
                self.media_pool.insert(id, item);
                self.bump_next_id(id.0);
                Ok(C::RemoveMedia { id })
            }
            C::RemoveMedia { id } => {
                if !self.media_pool.contains_key(&id) {
                    return Err(not_found("media", id.0));
                }
                if self
                    .clips
                    .values()
                    .any(|c| matches!(c.content, ClipContent::Audio { media, .. } if media == id))
                {
                    return Err(in_use("media", id.0));
                }
                let item = self.media_pool.remove(&id).expect("checked above");
                Ok(C::AddMedia { item })
            }
            C::ReplaceMedia { item } => {
                let id = item.id;
                if !self.media_pool.contains_key(&id) {
                    return Err(not_found("media", id.0));
                }
                check_media(&item)?;
                let old = self.media_pool.insert(id, item).expect("checked above");
                Ok(C::ReplaceMedia { item: old })
            }
            C::AddLane { lane } => {
                let id = lane.id;
                self.check_new_id(id.0)?;
                self.check_lane(&lane)?;
                self.automation.insert(id, lane);
                self.bump_next_id(id.0);
                Ok(C::RemoveLane { id })
            }
            C::RemoveLane { id } => {
                let lane = self
                    .automation
                    .remove(&id)
                    .ok_or_else(|| not_found("lane", id.0))?;
                Ok(C::AddLane { lane })
            }
            C::ReplaceLane { lane } => {
                let id = lane.id;
                if !self.automation.contains_key(&id) {
                    return Err(not_found("lane", id.0));
                }
                self.check_lane(&lane)?;
                let old = self.automation.insert(id, lane).expect("checked above");
                Ok(C::ReplaceLane { lane: old })
            }
            C::AddMarker { marker } => {
                let id = marker.id;
                self.check_new_id(id.0)?;
                check_marker(&marker)?;
                self.markers.insert(id, marker);
                self.bump_next_id(id.0);
                Ok(C::RemoveMarker { id })
            }
            C::RemoveMarker { id } => {
                let marker = self
                    .markers
                    .remove(&id)
                    .ok_or_else(|| not_found("marker", id.0))?;
                Ok(C::AddMarker { marker })
            }
            C::ReplaceMarker { marker } => {
                let id = marker.id;
                if !self.markers.contains_key(&id) {
                    return Err(not_found("marker", id.0));
                }
                check_marker(&marker)?;
                let old = self.markers.insert(id, marker).expect("checked above");
                Ok(C::ReplaceMarker { marker: old })
            }
            C::Batch { commands } => {
                let mut inverses = Vec::with_capacity(commands.len());
                for command in commands {
                    match self.apply(command) {
                        Ok(inverse) => inverses.push(inverse),
                        Err(error) => {
                            for inverse in inverses.into_iter().rev() {
                                // Inverses of commands that just succeeded always apply.
                                let undone = self.apply(inverse);
                                debug_assert!(undone.is_ok(), "batch rollback failed");
                            }
                            return Err(error);
                        }
                    }
                }
                inverses.reverse();
                Ok(C::Batch { commands: inverses })
            }
        }
    }

    fn check_new_id(&self, id: u64) -> Result<(), EditError> {
        if id > crate::validate::MAX_ID {
            Err(EditError::Invalid(ValidationError {
                path: "id".into(),
                message: format!("id {id} is above {}", crate::validate::MAX_ID),
            }))
        } else if self.id_in_use(id) {
            Err(EditError::IdTaken(id))
        } else {
            Ok(())
        }
    }

    fn bump_next_id(&mut self, id: u64) {
        self.next_id = self.next_id.max(id.saturating_add(1));
    }

    /// Rebuilds a strip with `change`, validates it and the routing, and installs it; returns
    /// the previous strip.
    fn replace_strip(
        &mut self,
        target: MixerRef,
        change: impl FnOnce(&mut Strip),
    ) -> Result<Strip, EditError> {
        let old = self
            .strip(target)
            .ok_or_else(|| match target {
                MixerRef::Track(id) => not_found("track", id.0),
                MixerRef::Bus(id) => not_found("bus", id.0),
                MixerRef::Master => not_found("master", 0),
            })?
            .clone();
        let mut new = old.clone();
        change(&mut new);
        self.check_strip("strip", &new, target == MixerRef::Master)?;
        // Inserts only change through device commands, which keep slot ownership consistent.
        new.inserts = old.inserts.clone();
        *self.strip_mut(target).expect("checked above") = new;
        if !self.routing_is_acyclic() {
            *self.strip_mut(target).expect("checked above") = old;
            return Err(EditError::RoutingCycle);
        }
        Ok(old)
    }

    /// Ids of the devices a track owns, in slot order.
    fn track_devices(track: &Track) -> Vec<DeviceId> {
        let mut ids = track.strip.inserts.clone();
        ids.extend(track.instrument);
        ids
    }

    /// Removes the devices `ids` and every lane targeting `strip` or one of them.
    fn extract_owned(
        &mut self,
        strip: MixerRef,
        ids: &[DeviceId],
    ) -> (Vec<Device>, Vec<AutomationLane>) {
        let set: HashSet<DeviceId> = ids.iter().copied().collect();
        let lane_ids: Vec<LaneId> = self
            .automation
            .values()
            .filter(|lane| match lane.target {
                AutomationTarget::Device { device, .. } => set.contains(&device),
                AutomationTarget::Strip { strip: s, .. } => s == strip,
            })
            .map(|lane| lane.id)
            .collect();
        let lanes = lane_ids
            .into_iter()
            .filter_map(|id| self.automation.remove(&id))
            .collect();
        let devices = ids
            .iter()
            .filter_map(|id| self.devices.remove(id))
            .collect();
        (devices, lanes)
    }

    /// Ids in `bundle` must be fresh and distinct; devices must be exactly the strip's slots and
    /// lanes must target the strip or its devices.
    #[allow(clippy::too_many_arguments)]
    fn check_bundle_shape(
        &self,
        own_id: u64,
        strip: &Strip,
        instrument: Option<DeviceId>,
        owner: MixerRef,
        devices: &[Device],
        lanes: &[AutomationLane],
        clips: &[Clip],
    ) -> Result<(), EditError> {
        let mut seen = HashSet::new();
        let ids = std::iter::once(own_id)
            .chain(devices.iter().map(|d| d.id.0))
            .chain(lanes.iter().map(|l| l.id.0))
            .chain(clips.iter().map(|c| c.id.0));
        for id in ids {
            self.check_new_id(id)?;
            if !seen.insert(id) {
                return Err(EditError::IdTaken(id));
            }
        }
        let mut slots: Vec<DeviceId> = strip.inserts.clone();
        slots.extend(instrument);
        let mut have: Vec<DeviceId> = devices.iter().map(|d| d.id).collect();
        slots.sort();
        have.sort();
        if slots != have || slots.windows(2).any(|w| w[0] == w[1]) {
            return Err(EditError::Invalid(ValidationError {
                path: "devices".into(),
                message: "devices must match the strip's slots exactly".into(),
            }));
        }
        for lane in lanes {
            let owned = match lane.target {
                AutomationTarget::Device { device, .. } => have.contains(&device),
                AutomationTarget::Strip { strip: s, .. } => s == owner,
            };
            if !owned {
                return Err(EditError::Invalid(ValidationError {
                    path: "lanes".into(),
                    message: "a lane targets something outside the bundle".into(),
                }));
            }
        }
        Ok(())
    }

    fn insert_track(&mut self, index: usize, bundle: Owned<Track>) -> Result<(), EditError> {
        if index > self.tracks.len() {
            return Err(EditError::IndexOutOfRange);
        }
        let Owned {
            item: track,
            clips,
            devices,
            lanes,
        } = bundle;
        let id = track.id;
        self.check_bundle_shape(
            id.0,
            &track.strip,
            track.instrument,
            MixerRef::Track(id),
            &devices,
            &lanes,
            &clips,
        )?;
        if clips.iter().any(|c| c.track != id) {
            return Err(EditError::Invalid(ValidationError {
                path: "clips".into(),
                message: "a clip belongs to another track".into(),
            }));
        }
        let max_id = std::iter::once(id.0)
            .chain(devices.iter().map(|d| d.id.0))
            .chain(lanes.iter().map(|l| l.id.0))
            .chain(clips.iter().map(|c| c.id.0))
            .max()
            .unwrap_or(0);
        for device in devices {
            self.devices.insert(device.id, device);
        }
        self.tracks.insert(index, track);
        for clip in clips {
            self.clips.insert(clip.id, clip);
        }
        for lane in lanes {
            self.automation.insert(lane.id, lane);
        }
        if let Err(error) = self.check_inserted_track(id) {
            // Roll back: take everything just inserted out again.
            let _ = self.remove_track_unchecked(id);
            return Err(error);
        }
        self.bump_next_id(max_id);
        Ok(())
    }

    fn check_inserted_track(&self, id: TrackId) -> Result<(), EditError> {
        let track = self.track(id).expect("just inserted");
        self.check_track(track)?;
        for clip in self.clips.values().filter(|c| c.track == id) {
            self.check_clip(clip)?;
        }
        for device in Self::track_devices(track) {
            self.check_device(&self.devices[&device])?;
        }
        for lane in self.automation.values() {
            self.check_lane(lane)?;
        }
        if !self.routing_is_acyclic() {
            return Err(EditError::RoutingCycle);
        }
        Ok(())
    }

    fn remove_track_unchecked(&mut self, id: TrackId) -> Option<(usize, Owned<Track>)> {
        let index = self.tracks.iter().position(|t| t.id == id)?;
        let track = self.tracks.remove(index);
        let clip_ids: Vec<ClipId> = self
            .clips
            .values()
            .filter(|c| c.track == id)
            .map(|c| c.id)
            .collect();
        let clips = clip_ids
            .into_iter()
            .filter_map(|c| self.clips.remove(&c))
            .collect();
        let ids = Self::track_devices(&track);
        let (devices, lanes) = self.extract_owned(MixerRef::Track(id), &ids);
        Some((
            index,
            Owned {
                item: track,
                clips,
                devices,
                lanes,
            },
        ))
    }

    fn remove_track(&mut self, id: TrackId) -> Result<(usize, Owned<Track>), EditError> {
        let track = self.track(id).ok_or_else(|| not_found("track", id.0))?;
        let owned: HashSet<DeviceId> = Self::track_devices(track).into_iter().collect();
        if self
            .devices
            .values()
            .any(|d| !owned.contains(&d.id) && d.sidechain == Some(MixerRef::Track(id)))
        {
            return Err(in_use("track", id.0));
        }
        Ok(self.remove_track_unchecked(id).expect("track exists"))
    }

    fn insert_bus(&mut self, index: usize, bundle: Owned<Bus>) -> Result<(), EditError> {
        if index > self.buses.len() {
            return Err(EditError::IndexOutOfRange);
        }
        let Owned {
            item: bus,
            clips,
            devices,
            lanes,
        } = bundle;
        if !clips.is_empty() {
            return Err(EditError::Invalid(ValidationError {
                path: "clips".into(),
                message: "a bus has no clips".into(),
            }));
        }
        let id = bus.id;
        self.check_bundle_shape(
            id.0,
            &bus.strip,
            None,
            MixerRef::Bus(id),
            &devices,
            &lanes,
            &clips,
        )?;
        let max_id = std::iter::once(id.0)
            .chain(devices.iter().map(|d| d.id.0))
            .chain(lanes.iter().map(|l| l.id.0))
            .max()
            .unwrap_or(0);
        let ids: Vec<DeviceId> = bus.strip.inserts.clone();
        for device in devices {
            self.devices.insert(device.id, device);
        }
        self.buses.insert(index, bus);
        for lane in lanes {
            self.automation.insert(lane.id, lane);
        }
        let checked = (|| {
            self.check_bus(self.bus(id).expect("just inserted"))?;
            for device in &ids {
                self.check_device(&self.devices[device])?;
            }
            for lane in self.automation.values() {
                self.check_lane(lane)?;
            }
            if !self.routing_is_acyclic() {
                return Err(EditError::RoutingCycle);
            }
            Ok(())
        })();
        if let Err(error) = checked {
            let _ = self.remove_bus_unchecked(id);
            return Err(error);
        }
        self.bump_next_id(max_id);
        Ok(())
    }

    fn remove_bus_unchecked(&mut self, id: BusId) -> Option<(usize, Owned<Bus>)> {
        let index = self.buses.iter().position(|b| b.id == id)?;
        let bus = self.buses.remove(index);
        let ids = bus.strip.inserts.clone();
        let (devices, lanes) = self.extract_owned(MixerRef::Bus(id), &ids);
        Some((
            index,
            Owned {
                item: bus,
                clips: Vec::new(),
                devices,
                lanes,
            },
        ))
    }

    fn remove_bus(&mut self, id: BusId) -> Result<(usize, Owned<Bus>), EditError> {
        let bus = self.bus(id).ok_or_else(|| not_found("bus", id.0))?;
        let owned: HashSet<DeviceId> = bus.strip.inserts.iter().copied().collect();
        let routed = |strip: &Strip| {
            strip.settings.output == crate::model::Output::Bus(id)
                || strip.sends.iter().any(|s| s.target == id)
        };
        if self.tracks.iter().any(|t| routed(&t.strip))
            || self.buses.iter().any(|b| b.id != id && routed(&b.strip))
            || self
                .devices
                .values()
                .any(|d| !owned.contains(&d.id) && d.sidechain == Some(MixerRef::Bus(id)))
        {
            return Err(in_use("bus", id.0));
        }
        Ok(self.remove_bus_unchecked(id).expect("bus exists"))
    }

    fn insert_device(&mut self, slot: DeviceSlot, bundle: DeviceBundle) -> Result<(), EditError> {
        let DeviceBundle { device, lanes } = bundle;
        let id = device.id;
        let mut seen = HashSet::new();
        for lid in std::iter::once(id.0).chain(lanes.iter().map(|l| l.id.0)) {
            self.check_new_id(lid)?;
            if !seen.insert(lid) {
                return Err(EditError::IdTaken(lid));
            }
        }
        if lanes
            .iter()
            .any(|l| !matches!(l.target, AutomationTarget::Device { device, .. } if device == id))
        {
            return Err(EditError::Invalid(ValidationError {
                path: "lanes".into(),
                message: "a lane does not target the device".into(),
            }));
        }
        match slot {
            DeviceSlot::Instrument { track } => {
                let t = self
                    .track(track)
                    .ok_or_else(|| not_found("track", track.0))?;
                if t.kind != crate::model::TrackKind::Midi {
                    return Err(EditError::WrongTarget(
                        "only MIDI tracks have an instrument",
                    ));
                }
                if t.instrument.is_some() {
                    return Err(EditError::SlotOccupied);
                }
            }
            DeviceSlot::Insert { strip, index } => {
                let s = self.strip(strip).ok_or_else(|| match strip {
                    MixerRef::Track(t) => not_found("track", t.0),
                    MixerRef::Bus(b) => not_found("bus", b.0),
                    MixerRef::Master => not_found("master", 0),
                })?;
                if index > s.inserts.len() {
                    return Err(EditError::IndexOutOfRange);
                }
            }
        }
        self.check_device(&device)?;
        let max_id = lanes
            .iter()
            .map(|l| l.id.0)
            .chain([id.0])
            .max()
            .unwrap_or(0);
        self.devices.insert(id, device);
        match slot {
            DeviceSlot::Instrument { track } => {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) {
                    t.instrument = Some(id);
                }
            }
            DeviceSlot::Insert { strip, index } => {
                if let Some(s) = self.strip_mut(strip) {
                    s.inserts.insert(index, id);
                }
            }
        }
        let lane_ids: Vec<LaneId> = lanes.iter().map(|l| l.id).collect();
        for lane in lanes {
            self.automation.insert(lane.id, lane);
        }
        for lane in &lane_ids {
            if let Err(error) = self.check_lane(&self.automation[lane]) {
                let _ = self.remove_device_unchecked(id);
                return Err(error.into());
            }
        }
        if !self.routing_is_acyclic() {
            let _ = self.remove_device_unchecked(id);
            return Err(EditError::RoutingCycle);
        }
        self.bump_next_id(max_id);
        Ok(())
    }

    fn remove_device_unchecked(&mut self, id: DeviceId) -> Option<(DeviceSlot, DeviceBundle)> {
        let slot = self.device_slot(id)?;
        match slot {
            DeviceSlot::Instrument { track } => {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track) {
                    t.instrument = None;
                }
            }
            DeviceSlot::Insert { strip, index } => {
                if let Some(s) = self.strip_mut(strip) {
                    s.inserts.remove(index);
                }
            }
        }
        let lane_ids: Vec<LaneId> = self
            .automation
            .values()
            .filter(|l| matches!(l.target, AutomationTarget::Device { device, .. } if device == id))
            .map(|l| l.id)
            .collect();
        let lanes = lane_ids
            .into_iter()
            .filter_map(|l| self.automation.remove(&l))
            .collect();
        let device = self.devices.remove(&id)?;
        Some((slot, DeviceBundle { device, lanes }))
    }

    fn remove_device(&mut self, id: DeviceId) -> Result<(DeviceSlot, DeviceBundle), EditError> {
        self.remove_device_unchecked(id)
            .ok_or_else(|| not_found("device", id.0))
    }

    /// Where `id` sits, if it exists.
    pub fn device_slot(&self, id: DeviceId) -> Option<DeviceSlot> {
        let find = |strip: MixerRef, s: &Strip| {
            s.inserts
                .iter()
                .position(|d| *d == id)
                .map(|index| DeviceSlot::Insert { strip, index })
        };
        for track in &self.tracks {
            if track.instrument == Some(id) {
                return Some(DeviceSlot::Instrument { track: track.id });
            }
            if let Some(slot) = find(MixerRef::Track(track.id), &track.strip) {
                return Some(slot);
            }
        }
        for bus in &self.buses {
            if let Some(slot) = find(MixerRef::Bus(bus.id), &bus.strip) {
                return Some(slot);
            }
        }
        find(MixerRef::Master, &self.master)
    }
}
