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
