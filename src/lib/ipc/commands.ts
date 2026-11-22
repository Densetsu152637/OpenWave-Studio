import type { IpcTransport } from "./transport";
import { tauriTransport } from "./transport";
import type { CommandReply, EditCommand, ModelPatch, Snapshot, UiCommand } from "./types";

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

/** Sends one command. Rejects with the reason when the model refused it. */
export function ipcCommand(
  command: UiCommand,
  transport: IpcTransport = tauriTransport,
): Promise<CommandReply> {
  return transport.invoke<CommandReply>("ipc_command", { command });
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
  const reply = await transport.invoke<CommandReply>("ipc_alloc_ids", { count });
  return reply.ids;
}
