import type { IpcTransport } from "./transport";
import { tauriTransport } from "./transport";
import type {
  AnalysisReport,
  AnalysisRequest,
  CommandReply,
  DeviceDescriptor,
  EditCommand,
  ExportOptions,
  ExportResult,
  MediaImportReply,
  ModelPatch,
  PluginScanSummary,
  Snapshot,
  UiCommand,
} from "./types";

/**
 * Typed wrappers over the project commands in `src-tauri/src/ipc/`. They send requests; the model
 * itself only changes through the patches the control thread emits, which the state mirror
 * (`$lib/stores/model`) applies. Nothing here mutates UI state.
 */

/** Registers `onPatch` as the patch channel and returns the snapshot patches continue from. */
export function ipcAttach(
  onPatch: (patch: ModelPatch) => void,
  transport: IpcTransport = tauriTransport,
): Promise<Snapshot> {
  return transport.invoke<Snapshot>("ipc_attach", { patches: transport.channel(onPatch) });
}

/** The whole project at the current sequence number (the answer to a detected gap). */
export function ipcSnapshot(transport: IpcTransport = tauriTransport): Promise<Snapshot> {
  return transport.invoke<Snapshot>("ipc_snapshot");
}

/** Per transport: the tail of the command chain. */
const queues = new WeakMap<IpcTransport, Promise<unknown>>();

function queued<T>(transport: IpcTransport, work: () => Promise<T>): Promise<T> {
  const previous = queues.get(transport) ?? Promise.resolve();
  const run = previous.then(work);
  queues.set(
    transport,
    run.catch(() => undefined),
  );
  return run;
}

/**
 * Sends one command. Rejects with the reason when the model refused it. Commands are serialized:
 * each is invoked only after the previous one settled, so begin, edit, end, undo and redo reach the
 * control thread in the order they were issued. A failed command does not block the ones after it.
 */
export function ipcCommand(
  command: UiCommand,
  transport: IpcTransport = tauriTransport,
): Promise<CommandReply> {
  return queued(transport, () => transport.invoke<CommandReply>("ipc_command", { command }));
}

/** Applies an edit as one undo step (or as part of the open transaction). */
export function edit(edit: EditCommand, transport?: IpcTransport): Promise<CommandReply> {
  return ipcCommand({ type: "edit", edit }, transport);
}

export const undo = (transport?: IpcTransport) => ipcCommand({ type: "undo" }, transport);
export const redo = (transport?: IpcTransport) => ipcCommand({ type: "redo" }, transport);
export const cancelTransaction = (transport?: IpcTransport) =>
  ipcCommand({ type: "cancel" }, transport);
export const endTransaction = (transport?: IpcTransport) => ipcCommand({ type: "end" }, transport);

/** Opens a transaction: the edits until `endTransaction` undo as one step named `label`. */
export function beginTransaction(label: string, transport?: IpcTransport): Promise<CommandReply> {
  return ipcCommand({ type: "begin", label }, transport);
}

/** Reserves `count` fresh entity ids for the entities the next edits create. */
export async function allocIds(count: number, transport: IpcTransport = tauriTransport): Promise<number[]> {
  // Same queue as the other commands, so ids are reserved in issue order too.
  const reply = await queued(transport, () =>
    transport.invoke<CommandReply>("ipc_alloc_ids", { count }),
  );
  return reply.ids;
}

/**
 * Starts a new, empty, unsaved project. The returned snapshot is the state the mirror also gets
 * as a reset patch. Project, media and export commands go through the same queue as edits, so
 * they never overtake one.
 */
export function projectNew(transport: IpcTransport = tauriTransport): Promise<Snapshot> {
  return queued(transport, () => transport.invoke<Snapshot>("project_new"));
}

/** Opens the `.owave` bundle at `path`. Rejects with the reason when it cannot be opened. */
export function projectOpen(
  path: string,
  transport: IpcTransport = tauriTransport,
): Promise<Snapshot> {
  return queued(transport, () => transport.invoke<Snapshot>("project_open", { path }));
}

/**
 * Saves the project to the bundle `path` (".owave" is added when it has no extension), or, with no
 * path, to the bundle it came from (rejects when it never was saved). Resolves with the bundle's
 * path; the mirror's `projectPath` and `history.dirty` follow as patches.
 */
export function projectSave(
  path?: string,
  transport: IpcTransport = tauriTransport,
): Promise<string> {
  return queued(transport, () =>
    transport.invoke<string>("project_save", { path: path ?? null }),
  );
}

/**
 * Imports the audio file at `path` into the project's media pool at the project's sample rate
 * (decode, resample, content-hashed copy, waveform). The backend adds the media item; the caller
 * then adds clips with `edit({ command: "add_clip", ... })`. Importing the same content twice
 * answers the existing item.
 */
export function mediaImport(
  path: string,
  transport: IpcTransport = tauriTransport,
): Promise<MediaImportReply> {
  return queued(transport, () => transport.invoke<MediaImportReply>("media_import", { path }));
}

/**
 * Renders the project to a 32-bit float stereo WAV at `path` (".wav" is added when it has no
 * extension): from 0 to the end of the last clip plus one second. Resolves with the path written.
 */
export function exportWav(path: string, transport: IpcTransport = tauriTransport): Promise<string> {
  return queued(transport, () => transport.invoke<string>("export_wav", { path }));
}

/**
 * Renders the project like `exportWav`, optionally scaled to an integrated loudness under a
 * true-peak ceiling (`options.normalize`), and answers where the file went, what normalising did
 * and the measurements of what was written.
 */
export function exportMix(
  options: ExportOptions,
  transport: IpcTransport = tauriTransport,
): Promise<ExportResult> {
  return queued(transport, () => transport.invoke<ExportResult>("export_mix", { options }));
}

/**
 * Renders the mix (or one strip after its fader, or a beat range) offline and measures it:
 * loudness, true and sample peak, RMS, crest factor, stereo correlation and eight band energies.
 */
export function analyze(
  request: AnalysisRequest = { strip: null, startBeat: null, endBeat: null },
  transport: IpcTransport = tauriTransport,
): Promise<AnalysisReport> {
  return queued(transport, () => transport.invoke<AnalysisReport>("analyze", { request }));
}

/**
 * Arms or disarms an audio track for recording. Arming is session state: the mirror's
 * `history.armedTracks` follows as a patch; it is not an edit (no undo, no dirty flag). Rejects
 * for a track that cannot record and while a recording is running.
 */
export function recordSetArm(
  track: number,
  armed: boolean,
  transport: IpcTransport = tauriTransport,
): Promise<void> {
  return queued(transport, () => transport.invoke<void>("record_set_arm", { track, armed }));
}

/**
 * Starts recording on the armed tracks from the playhead. Stop with `transportStop`, which adds
 * the takes to the project as clips (one undo step). Rejects when no track is armed or no audio
 * input is open.
 */
export function recordStart(transport: IpcTransport = tauriTransport): Promise<void> {
  return queued(transport, () => transport.invoke<void>("record_start"));
}

/**
 * Scans the plugin folders (the standard ones, `CLAP_PATH` and the saved extra folders) for CLAP
 * plugins. Each file is loaded in a separate process, so a plugin that crashes or hangs is
 * skipped and reported in `failures`. Unchanged files come from the cache unless `force`. Not
 * queued behind edits: a scan can take seconds. The backend also emits `plugins-changed`.
 */
export function pluginsScan(
  force = false,
  transport: IpcTransport = tauriTransport,
): Promise<PluginScanSummary> {
  return transport.invoke<PluginScanSummary>("plugins_scan", { force });
}

/** What the last scan found (usable plugins and skipped files), without scanning again. */
export function pluginsList(
  transport: IpcTransport = tauriTransport,
): Promise<PluginScanSummary> {
  return transport.invoke<PluginScanSummary>("plugins_list");
}

/**
 * Every device kind with its parameter descriptors (id, name, range, default, unit, scale,
 * choice labels): the built-in devices, then the scanned plugins. What the generic device panel
 * and the effect and instrument pickers are built from. It changes when the plugin list does.
 */
export function deviceDescriptors(
  transport: IpcTransport = tauriTransport,
): Promise<DeviceDescriptor[]> {
  return transport.invoke<DeviceDescriptor[]>("device_descriptors");
}
