//! Computing patches ([`diff`]) and the reference state mirror ([`Mirror`]).
//!
//! [`Mirror`] is the Rust twin of the UI's store (`src/lib/stores/`): both implement the same
//! rules, and the `mirror` tests check the Rust one against the real model. The TypeScript one is
//! checked against a stream this crate generates (`src/lib/stores/fixtures/`).

use std::collections::BTreeMap;
use std::fmt;

use ow_model::{Keyed, Project};

use crate::dto::{HistoryState, ModelPatch, PatchOp, Snapshot};

/// Operations that turn `before` into `after`. Empty when the projects are equal.
pub fn diff(before: &Project, after: &Project) -> Vec<PatchOp> {
    let mut ops = Vec::new();
    if before.name != after.name {
        ops.push(PatchOp::SetName {
            name: after.name.clone(),
        });
    }
    if before.settings != after.settings {
        ops.push(PatchOp::SetSettings {
            settings: after.settings.clone(),
        });
    }
    if before.tempo_map != after.tempo_map {
        ops.push(PatchOp::SetTempoMap {
            tempo_map: after.tempo_map.clone(),
        });
    }
    if before.time_signature_map != after.time_signature_map {
        ops.push(PatchOp::SetTimeSignatureMap {
            map: after.time_signature_map.clone(),
        });
    }
    if before.master != after.master {
        ops.push(PatchOp::SetMaster {
            master: after.master.clone(),
        });
    }

    list_ops(
        &before.tracks,
        &after.tracks,
        &mut ops,
        |track| PatchOp::UpsertTrack { track },
        |id| PatchOp::RemoveTrack { id },
        |ids| PatchOp::OrderTracks { ids },
        |t| t.id,
    );
    list_ops(
        &before.buses,
        &after.buses,
        &mut ops,
        |bus| PatchOp::UpsertBus { bus },
        |id| PatchOp::RemoveBus { id },
        |ids| PatchOp::OrderBuses { ids },
        |b| b.id,
    );
    map_ops(
        &before.clips,
        &after.clips,
        &mut ops,
        |clip| PatchOp::UpsertClip { clip },
        |id| PatchOp::RemoveClip { id },
    );
    map_ops(
        &before.patterns,
        &after.patterns,
        &mut ops,
        |pattern| PatchOp::UpsertPattern { pattern },
        |id| PatchOp::RemovePattern { id },
    );
    map_ops(
        &before.media_pool,
        &after.media_pool,
        &mut ops,
        |item| PatchOp::UpsertMedia { item },
        |id| PatchOp::RemoveMedia { id },
    );
    map_ops(
        &before.devices,
        &after.devices,
        &mut ops,
        |device| PatchOp::UpsertDevice { device },
        |id| PatchOp::RemoveDevice { id },
    );
    map_ops(
        &before.automation,
        &after.automation,
        &mut ops,
        |lane| PatchOp::UpsertLane { lane },
        |id| PatchOp::RemoveLane { id },
    );
    map_ops(
        &before.markers,
        &after.markers,
        &mut ops,
        |marker| PatchOp::UpsertMarker { marker },
        |id| PatchOp::RemoveMarker { id },
    );
    if before.next_id != after.next_id {
        ops.push(PatchOp::SetNextId {
            next_id: after.next_id,
        });
    }
    ops
}

fn map_ops<T, I>(
    before: &BTreeMap<I, T>,
    after: &BTreeMap<I, T>,
    ops: &mut Vec<PatchOp>,
    upsert: impl Fn(T) -> PatchOp,
    remove: impl Fn(I) -> PatchOp,
) where
    T: Clone + PartialEq,
    I: Copy + Ord,
{
    for id in before.keys() {
        if !after.contains_key(id) {
            ops.push(remove(*id));
        }
    }
    for (id, item) in after {
        if before.get(id) != Some(item) {
            ops.push(upsert(item.clone()));
        }
    }
}

fn list_ops<T, I>(
    before: &[T],
    after: &[T],
    ops: &mut Vec<PatchOp>,
    upsert: impl Fn(T) -> PatchOp,
    remove: impl Fn(I) -> PatchOp,
    order: impl Fn(Vec<I>) -> PatchOp,
    id_of: impl Fn(&T) -> I,
) where
    T: Clone + PartialEq,
    I: Copy + Ord,
{
    let before_by_id: BTreeMap<I, &T> = before.iter().map(|t| (id_of(t), t)).collect();
    let after_ids: Vec<I> = after.iter().map(&id_of).collect();
    for t in before {
        let id = id_of(t);
        if !after_ids.contains(&id) {
            ops.push(remove(id));
        }
    }
    for t in after {
        if before_by_id.get(&id_of(t)).copied() != Some(t) {
            ops.push(upsert(t.clone()));
        }
    }
    // What a mirror holds after the ops above: survivors in their old order, then new entries.
    let predicted: Vec<I> = before
        .iter()
        .map(&id_of)
        .filter(|id| after_ids.contains(id))
        .chain(
            after_ids
                .iter()
                .copied()
                .filter(|id| !before_by_id.contains_key(id)),
        )
        .collect();
    if predicted != after_ids {
        ops.push(order(after_ids));
    }
}

/// A patch arrived out of order: the mirror needs a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    /// The sequence number the mirror needed next.
    pub expected: u64,
    /// The one it got.
    pub got: u64,
}

impl fmt::Display for Gap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "patch {} arrived, expected {}", self.got, self.expected)
    }
}

impl std::error::Error for Gap {}

/// A read-only copy of the control thread's project, kept in step by snapshots and patches.
#[derive(Debug, Clone, PartialEq)]
pub struct Mirror {
    /// Sequence number of the last patch applied.
    pub seq: u64,
    /// The mirrored project.
    pub project: Project,
    /// Undo and redo state.
    pub history: HistoryState,
    /// The `.owave` bundle the project is saved in, if it is.
    pub project_path: Option<String>,
}

impl Mirror {
    /// A mirror holding `snapshot`.
    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        Self {
            seq: snapshot.seq,
            project: snapshot.project,
            history: snapshot.history,
            project_path: snapshot.project_path,
        }
    }

    /// Replaces everything with `snapshot` (a resync).
    pub fn resync(&mut self, snapshot: Snapshot) {
        *self = Self::from_snapshot(snapshot);
    }

    /// Applies the next patch. A patch that is not exactly `seq + 1` leaves the mirror untouched
    /// and returns the [`Gap`]; a patch at or below `seq` is a duplicate and is ignored.
    pub fn apply(&mut self, patch: &ModelPatch) -> Result<(), Gap> {
        if patch.seq <= self.seq {
            return Ok(());
        }
        if patch.seq != self.seq + 1 {
            return Err(Gap {
                expected: self.seq + 1,
                got: patch.seq,
            });
        }
        for op in &patch.ops {
            match op {
                PatchOp::SetProjectPath { project_path } => {
                    self.project_path.clone_from(project_path);
                }
                _ => apply_op(&mut self.project, op),
            }
        }
        self.seq = patch.seq;
        self.history = patch.history.clone();
        Ok(())
    }
}

fn upsert_keyed<T: Keyed + Clone>(map: &mut BTreeMap<T::Id, T>, item: &T) {
    map.insert(item.id(), item.clone());
}

fn upsert_list<T: Clone, I: PartialEq>(list: &mut Vec<T>, item: &T, id_of: impl Fn(&T) -> I) {
    let id = id_of(item);
    match list.iter_mut().find(|t| id_of(t) == id) {
        Some(slot) => *slot = item.clone(),
        None => list.push(item.clone()),
    }
}

fn reorder<T, I: PartialEq>(list: &mut Vec<T>, ids: &[I], id_of: impl Fn(&T) -> I) {
    let mut rest = std::mem::take(list);
    for id in ids {
        if let Some(position) = rest.iter().position(|t| id_of(t) == *id) {
            list.push(rest.remove(position));
        }
    }
    // An id the order did not name keeps its place at the end (cannot happen with a valid patch).
    list.extend(rest);
}

fn apply_op(project: &mut Project, op: &PatchOp) {
    match op {
        PatchOp::Reset { project: new } => *project = (**new).clone(),
        // Not part of the project: the mirror keeps it next to the project.
        PatchOp::SetProjectPath { .. } => {}
        PatchOp::SetName { name } => project.name.clone_from(name),
        PatchOp::SetSettings { settings } => project.settings = settings.clone(),
        PatchOp::SetTempoMap { tempo_map } => project.tempo_map = tempo_map.clone(),
        PatchOp::SetTimeSignatureMap { map } => project.time_signature_map = map.clone(),
        PatchOp::SetMaster { master } => project.master = master.clone(),
        PatchOp::SetNextId { next_id } => project.next_id = *next_id,
        PatchOp::UpsertTrack { track } => upsert_list(&mut project.tracks, track, |t| t.id),
        PatchOp::RemoveTrack { id } => project.tracks.retain(|t| t.id != *id),
        PatchOp::OrderTracks { ids } => reorder(&mut project.tracks, ids, |t| t.id),
        PatchOp::UpsertBus { bus } => upsert_list(&mut project.buses, bus, |b| b.id),
        PatchOp::RemoveBus { id } => project.buses.retain(|b| b.id != *id),
        PatchOp::OrderBuses { ids } => reorder(&mut project.buses, ids, |b| b.id),
        PatchOp::UpsertClip { clip } => upsert_keyed(&mut project.clips, clip),
        PatchOp::RemoveClip { id } => {
            project.clips.remove(id);
        }
        PatchOp::UpsertPattern { pattern } => upsert_keyed(&mut project.patterns, pattern),
        PatchOp::RemovePattern { id } => {
            project.patterns.remove(id);
        }
        PatchOp::UpsertMedia { item } => upsert_keyed(&mut project.media_pool, item),
        PatchOp::RemoveMedia { id } => {
            project.media_pool.remove(id);
        }
        PatchOp::UpsertDevice { device } => upsert_keyed(&mut project.devices, device),
        PatchOp::RemoveDevice { id } => {
            project.devices.remove(id);
        }
        PatchOp::UpsertLane { lane } => upsert_keyed(&mut project.automation, lane),
        PatchOp::RemoveLane { id } => {
            project.automation.remove(id);
        }
        PatchOp::UpsertMarker { marker } => upsert_keyed(&mut project.markers, marker),
        PatchOp::RemoveMarker { id } => {
            project.markers.remove(id);
        }
    }
}
