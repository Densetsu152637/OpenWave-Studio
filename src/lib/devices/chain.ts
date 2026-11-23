import type { ArrangeApi } from "../arrange/edits";
import type { Device, DeviceDescriptor, EditCommand, MixerRef, Strip, Track } from "../ipc/types";
import { newDevice } from "./params";

/**
 * View-model of a track's device chain: its instrument (MIDI tracks) followed by its inserts in
 * signal order, and the edits that add, remove and bypass an insert. The model holds the chain
 * (`Track.instrument`, `Strip.inserts`); nothing here keeps state.
 */

/** One device in a track's chain. */
export interface ChainEntry {
  device: Device;
  /** The track's sound source, or an effect inserted after it. */
  role: "instrument" | "insert";
}

/** The track's devices in signal order; an id the project does not hold is skipped. */
export function chainOf(
  track: Pick<Track, "instrument" | "strip">,
  devices: ReadonlyMap<number, Device>,
): ChainEntry[] {
  const entries: ChainEntry[] = [];
  const instrument = track.instrument == null ? undefined : devices.get(track.instrument);
  if (instrument) entries.push({ device: instrument, role: "instrument" });
  for (const id of track.strip.inserts) {
    const device = devices.get(id);
    if (device) entries.push({ device, role: "insert" });
  }
  return entries;
}

/** The kinds that can be added as an insert, in the backend's order. */
export const effectDescriptors = (descriptors: readonly DeviceDescriptor[]): DeviceDescriptor[] =>
  descriptors.filter((d) => !d.instrument);

/**
 * The chain entry whose panel is shown: the one with id `selected`, else the first (a MIDI
 * track's instrument), else none. A stale id (a removed device, another track's) falls back.
 */
export function shownEntry(
  chain: readonly ChainEntry[],
  selected: number | null,
): ChainEntry | undefined {
  return chain.find((entry) => entry.device.id === selected) ?? chain[0];
}

/** The inserts of a bus or the master strip (they have no instrument), in signal order. */
export const stripChainOf = (
  strip: Strip,
  devices: ReadonlyMap<number, Device>,
): ChainEntry[] => chainOf({ instrument: null, strip }, devices);

/**
 * `InsertDevice` appending `descriptor`'s device (id `id`, default parameters) to the end of the
 * insert chain of `ref` (a track, a bus or the master), whose strip is `strip`.
 */
export function addStripInsertCommand(
  ref: MixerRef,
  strip: Pick<Strip, "inserts">,
  descriptor: DeviceDescriptor,
  id: number,
): EditCommand {
  return {
    command: "insert_device",
    slot: { type: "insert", strip: ref, index: strip.inserts.length },
    bundle: { device: newDevice(descriptor, id), lanes: [] },
  };
}

/** `InsertDevice` appending `descriptor`'s device (id `id`, default parameters) to the track. */
export const addInsertCommand = (
  track: Pick<Track, "id" | "strip">,
  descriptor: DeviceDescriptor,
  id: number,
): EditCommand => addStripInsertCommand({ track: track.id }, track.strip, descriptor, id);

/** `RemoveDevice` for an insert. */
export const removeInsertCommand = (device: Pick<Device, "id">): EditCommand => ({
  command: "remove_device",
  id: device.id,
});

/** `SetDeviceEnabled` flipping a device between active and bypassed. */
export const toggleBypassCommand = (device: Pick<Device, "id" | "enabled">): EditCommand => ({
  command: "set_device_enabled",
  device: device.id,
  enabled: !device.enabled,
});

/** Appends a new effect to a strip's chain (a track, bus or the master); resolves with its id. */
export async function addStripInsert(
  api: ArrangeApi,
  ref: MixerRef,
  strip: Pick<Strip, "inserts">,
  descriptor: DeviceDescriptor,
): Promise<number> {
  if (descriptor.instrument) throw new Error(`${descriptor.name} is an instrument, not an effect`);
  const [id] = await api.allocIds(1);
  if (id === undefined) throw new Error("no id was reserved");
  await api.edit(addStripInsertCommand(ref, strip, descriptor, id));
  return id;
}

/** Appends a new effect to the track's chain; resolves with the new device's id. */
export const addInsert = (
  api: ArrangeApi,
  track: Pick<Track, "id" | "strip">,
  descriptor: DeviceDescriptor,
): Promise<number> => addStripInsert(api, { track: track.id }, track.strip, descriptor);
