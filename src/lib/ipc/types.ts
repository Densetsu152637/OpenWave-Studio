/**
 * The IPC DTOs as the UI sees them. They are generated from the Rust types by `ts-rs` into
 * `./generated/` (committed; `cargo test -p ow-ipc export_bindings` regenerates them and CI checks
 * they are current), and re-exported here so the rest of the UI has one import path.
 *
 * The project JSON is the `project.json` shape: keyed collections (clips, patterns, media, devices,
 * automation, markers) are arrays on the wire. The state mirror (`$lib/stores/model`) turns them
 * into maps keyed by id.
 */
export type { AutomationLane } from "./generated/AutomationLane";
export type { AutomationPoint } from "./generated/AutomationPoint";
export type { AutomationTarget } from "./generated/AutomationTarget";
export type { AuxSend } from "./generated/AuxSend";
export type { BeatRange } from "./generated/BeatRange";
export type { Bus } from "./generated/Bus";
export type { BusId } from "./generated/BusId";
export type { Clip } from "./generated/Clip";
export type { ClipContent } from "./generated/ClipContent";
export type { ClipId } from "./generated/ClipId";
export type { CommandReply } from "./generated/CommandReply";
export type { Controller } from "./generated/Controller";
export type { Device } from "./generated/Device";
export type { DeviceDescriptor } from "./generated/DeviceDescriptor";
export type { DeviceBundle } from "./generated/DeviceBundle";
export type { DeviceId } from "./generated/DeviceId";
export type { DeviceSlot } from "./generated/DeviceSlot";
export type { EditCommand } from "./generated/EditCommand";
export type { HistoryState } from "./generated/HistoryState";
export type { LaneId } from "./generated/LaneId";
export type { Marker } from "./generated/Marker";
export type { MarkerId } from "./generated/MarkerId";
export type { MediaId } from "./generated/MediaId";
export type { MediaImportReply } from "./generated/MediaImportReply";
export type { MediaItem } from "./generated/MediaItem";
export type { MixerRef } from "./generated/MixerRef";
export type { ModelPatch } from "./generated/ModelPatch";
export type { Note } from "./generated/Note";
export type { Output } from "./generated/Output";
export type { Owned } from "./generated/Owned";
export type { PatchOp } from "./generated/PatchOp";
export type { ParamDescriptorDto } from "./generated/ParamDescriptorDto";
export type { ParamScale } from "./generated/ParamScale";
export type { ParamUnit } from "./generated/ParamUnit";
export type { Pattern } from "./generated/Pattern";
export type { PatternId } from "./generated/PatternId";
export type { Project } from "./generated/Project";
export type { ProjectSettings } from "./generated/ProjectSettings";
export type { SignatureChangeJson } from "./generated/SignatureChangeJson";
export type { Snapshot } from "./generated/Snapshot";
export type { StateChunk } from "./generated/StateChunk";
export type { Strip } from "./generated/Strip";
export type { StripParam } from "./generated/StripParam";
export type { StripSettings } from "./generated/StripSettings";
export type { TempoPointJson } from "./generated/TempoPointJson";
export type { Track } from "./generated/Track";
export type { TrackId } from "./generated/TrackId";
export type { TrackKind } from "./generated/TrackKind";
export type { UiCommand } from "./generated/UiCommand";
