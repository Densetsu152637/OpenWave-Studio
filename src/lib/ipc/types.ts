/**
 * The IPC DTOs as the UI sees them. They are generated from the Rust types by `ts-rs` into
 * `./generated/` (committed; `cargo test -p ow-ipc export_bindings` regenerates them and CI checks
 * they are current), and re-exported here so the rest of the UI has one import path.
 *
 * The project JSON is the `project.json` shape: keyed collections (clips, patterns, media, devices,
 * automation, markers) are arrays on the wire. The state mirror (`$lib/stores/model`) turns them
 * into maps keyed by id.
 */
export type { AnalysisReport } from "./generated/AnalysisReport";
export type { AnalysisRequest } from "./generated/AnalysisRequest";
export type { AutomationLane } from "./generated/AutomationLane";
export type { AutomationPoint } from "./generated/AutomationPoint";
export type { AutomationTarget } from "./generated/AutomationTarget";
export type { AuxSend } from "./generated/AuxSend";
export type { BandEnergy } from "./generated/BandEnergy";
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
export type { PluginScanFailure } from "./generated/PluginScanFailure";
export type { PluginScanSummary } from "./generated/PluginScanSummary";
export type { DeviceBundle } from "./generated/DeviceBundle";
export type { DeviceId } from "./generated/DeviceId";
export type { DeviceSlot } from "./generated/DeviceSlot";
export type { EditCommand } from "./generated/EditCommand";
export type { ExportOptions } from "./generated/ExportOptions";
export type { ExportResult } from "./generated/ExportResult";
export type { HistoryState } from "./generated/HistoryState";
export type { InputChannels } from "./generated/InputChannels";
export type { LaneId } from "./generated/LaneId";
export type { Marker } from "./generated/Marker";
export type { MarkerId } from "./generated/MarkerId";
export type { MediaId } from "./generated/MediaId";
export type { KeyInfo } from "./generated/KeyInfo";
export type { LibraryEntryInfo } from "./generated/LibraryEntryInfo";
export type { LibraryKey } from "./generated/LibraryKey";
export type { LibraryPreviewReply } from "./generated/LibraryPreviewReply";
export type { LibraryQuery } from "./generated/LibraryQuery";
export type { LibraryScanAction } from "./generated/LibraryScanAction";
export type { LibraryScanStatus } from "./generated/LibraryScanStatus";
export type { LibrarySearchReply } from "./generated/LibrarySearchReply";
export type { MediaImportReply } from "./generated/MediaImportReply";
export type { MediaItem } from "./generated/MediaItem";
export type { MonitorMode } from "./generated/MonitorMode";
export type { MixerRef } from "./generated/MixerRef";
export type { ModelPatch } from "./generated/ModelPatch";
export type { NormalizationReport } from "./generated/NormalizationReport";
export type { NormalizeOptions } from "./generated/NormalizeOptions";
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
export type { TakeInfo } from "./generated/TakeInfo";
export type { TempoPointJson } from "./generated/TempoPointJson";
export type { Track } from "./generated/Track";
export type { TrackInput } from "./generated/TrackInput";
export type { TrackId } from "./generated/TrackId";
export type { TrackKind } from "./generated/TrackKind";
export type { UiCommand } from "./generated/UiCommand";
export type { Warp } from "./generated/Warp";
export type { WarpMarker } from "./generated/WarpMarker";
export type { WarpMode } from "./generated/WarpMode";
