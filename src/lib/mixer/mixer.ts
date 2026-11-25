import type { ArrangeApi } from "../arrange/edits";
import { TRACK_COLORS, inTransaction, validName } from "../arrange/edits";
import type { MirrorProject } from "../stores/apply";
import type {
  AuxSend,
  Bus,
  EditCommand,
  MixerRef,
  Output,
  Owned,
  Strip,
  StripSettings,
} from "../ipc/types";

/**
 * View-model of the mixer controls: pan, solo, buses, output routing, sends and the master
 * strip. The model holds everything (`Strip.settings`, `Strip.sends`, `Project.buses`); these are
 * pure helpers that read it and build the edits, so nothing here keeps state.
 */

/** Lowest and highest fader value, in dB. The lowest reads as "-inf". */
export const MIN_DB = -60;
export const MAX_DB = 6;

/** Send level range, in dB. */
export const MIN_SEND_DB = -60;
export const MAX_SEND_DB = 6;

/** The strip a reference names, or undefined when the project no longer has it. */
export function stripOf(
  project: Pick<MirrorProject, "tracks" | "buses" | "master">,
  ref: MixerRef,
): Strip | undefined {
  if (ref === "master") return project.master;
  if ("track" in ref) return project.tracks.find((t) => t.id === ref.track)?.strip;
  return project.buses.find((b) => b.id === ref.bus)?.strip;
}

/** True when `a` and `b` name the same strip. */
export function sameStrip(a: MixerRef, b: MixerRef): boolean {
  if (a === "master" || b === "master") return a === b;
  if ("track" in a) return "track" in b && a.track === b.track;
  return "bus" in b && a.bus === b.bus;
}

/** "C", "L40" or "R25": the pan as a mixer shows it (-1 is "L100", 1 is "R100"). */
export function panLabel(pan: number): string {
  if (!Number.isFinite(pan)) return "C";
  const percent = Math.round(Math.min(1, Math.max(-1, pan)) * 100);
  if (percent === 0) return "C";
  return percent < 0 ? `L${-percent}` : `R${percent}`;
}

/** A fader or send level as text: "-inf" at the bottom of the range, else one decimal and "dB". */
export function dbLabel(db: number, floor: number = MIN_DB): string {
  return db <= floor ? "-inf dB" : `${db.toFixed(1)} dB`;
}

/** `pan` limited to -1..1 and snapped to the centre when it is within 2 % of it. */
export function snapPan(pan: number): number {
  if (!Number.isFinite(pan)) return 0;
  const limited = Math.min(1, Math.max(-1, pan));
  return Math.abs(limited) < 0.02 ? 0 : limited;
}

/** `SetStripSettings` for `strip` with the given fields changed and the rest kept. */
export function setStripCommand(
  strip: MixerRef,
  current: StripSettings,
  change: Partial<StripSettings>,
): EditCommand {
  return {
    command: "set_strip_settings",
    target: strip,
    settings: { ...current, ...change },
  };
}

// ---------------------------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------------------------

/** The buses a strip feeds directly: its output bus, if any, and its send targets. */
function feeds(strip: Strip): number[] {
  const targets = strip.sends.map((s) => s.target);
  if (strip.settings.output !== "master") targets.push(strip.settings.output.bus);
  return targets;
}

/**
 * True when audio leaving bus `from` (through its output or sends, to any depth) reaches bus
 * `to`, or when they are the same bus.
 */
export function busReaches(
  project: Pick<MirrorProject, "buses">,
  from: number,
  to: number,
): boolean {
  const seen = new Set<number>();
  const stack = [from];
  while (stack.length > 0) {
    const id = stack.pop()!;
    if (id === to) return true;
    if (seen.has(id)) continue;
    seen.add(id);
    const bus = project.buses.find((b) => b.id === id);
    if (bus) stack.push(...feeds(bus.strip));
  }
  return false;
}

/**
 * The buses `strip` could be routed or sent to without a cycle: every bus for a track, and for a
 * bus every other bus that does not (directly or not) feed it. The master has no destinations.
 */
export function routeTargets(
  project: Pick<MirrorProject, "buses">,
  strip: MixerRef,
): Bus[] {
  if (strip === "master") return [];
  if ("track" in strip) return [...project.buses];
  return project.buses.filter((bus) => !busReaches(project, bus.id, strip.bus));
}

/** `SetStripSettings` routing a strip's output to the master or to a bus. */
export function setOutputCommand(
  strip: MixerRef,
  current: StripSettings,
  output: Output,
): EditCommand {
  return setStripCommand(strip, current, { output });
}

/** The routing as a `<select>` value: "master" or the bus id. */
export const outputValue = (output: Output): string =>
  output === "master" ? "master" : String(output.bus);

/** The inverse of [`outputValue`]; null for a value that is neither. */
export function parseOutput(value: string): Output | null {
  if (value === "master") return "master";
  if (!/^\d+$/.test(value)) return null;
  const id = Number(value);
  return Number.isSafeInteger(id) ? { bus: id } : null;
}

// ---------------------------------------------------------------------------------------------
// Buses
// ---------------------------------------------------------------------------------------------

/** A new empty bus as the `InsertBus` bundle. */
export function newBus(id: number, name: string, color: number): Owned<Bus> {
  return {
    item: {
      id,
      name,
      color,
      strip: {
        settings: {
          gain_db: 0,
          pan: 0,
          mute: false,
          solo: false,
          solo_safe: false,
          invert_phase: false,
          output: "master",
        },
        sends: [],
        inserts: [],
      },
    },
    clips: [],
    devices: [],
    lanes: [],
  };
}

/** Appends an empty bus named "Bus N"; resolves with its id. */
export async function addBus(api: ArrangeApi, busCount: number): Promise<number> {
  const [id] = await api.allocIds(1);
  if (id === undefined) throw new Error("no bus id was reserved");
  await api.edit({
    command: "insert_bus",
    index: busCount,
    bundle: newBus(id, `Bus ${busCount + 1}`, TRACK_COLORS[(busCount + 3) % TRACK_COLORS.length]!),
  });
  return id;
}

/** Renames a bus; false (and nothing sent) for an unchanged or unacceptable name. */
export async function renameBus(api: ArrangeApi, bus: Bus, name: string): Promise<boolean> {
  const next = name.trim();
  if (next === bus.name || !validName(next)) return false;
  await api.edit({ command: "set_info", target: { bus: bus.id }, name: next, color: bus.color });
  return true;
}

/** Recolors a bus (`0xRRGGBB`); false (and nothing sent) when it already has that color. */
export async function setBusColor(api: ArrangeApi, bus: Bus, color: number): Promise<boolean> {
  if (color === bus.color) return false;
  await api.edit({ command: "set_info", target: { bus: bus.id }, name: bus.name, color });
  return true;
}

/** Moves a bus to index `to` (one `MoveBus`, one undo step). */
export const moveBus = (api: ArrangeApi, id: number, to: number) =>
  api.edit({ command: "move_bus", id, to });

/** Everything that still routes to bus `id`: the strips whose output or sends name it. */
export function busUsers(
  project: Pick<MirrorProject, "tracks" | "buses">,
  id: number,
): MixerRef[] {
  const users: MixerRef[] = [];
  const uses = (strip: Strip) =>
    (strip.settings.output !== "master" && strip.settings.output.bus === id) ||
    strip.sends.some((s) => s.target === id);
  for (const track of project.tracks) if (uses(track.strip)) users.push({ track: track.id });
  for (const bus of project.buses) if (bus.id !== id && uses(bus.strip)) users.push({ bus: bus.id });
  return users;
}

/**
 * Deletes a bus after rerouting what feeds it: every strip that outputs to it goes to the
 * master instead, and every send to it is removed, all inside one undo step. The bus's own
 * inserts go with it (the model removes what a bus owns).
 */
export async function removeBus(
  api: ArrangeApi,
  project: Pick<MirrorProject, "tracks" | "buses">,
  id: number,
): Promise<void> {
  const edits: EditCommand[] = [];
  const strips: [MixerRef, Strip][] = [
    ...project.tracks.map((t): [MixerRef, Strip] => [{ track: t.id }, t.strip]),
    ...project.buses.filter((b) => b.id !== id).map((b): [MixerRef, Strip] => [{ bus: b.id }, b.strip]),
  ];
  for (const [ref, strip] of strips) {
    const settings = strip.settings;
    if (settings.output !== "master" && settings.output.bus === id) {
      edits.push(setOutputCommand(ref, settings, "master"));
    }
    if (strip.sends.some((s) => s.target === id)) {
      edits.push(setSendsCommand(ref, strip.sends.filter((s) => s.target !== id)));
    }
  }
  edits.push({ command: "remove_bus", id });
  await inTransaction(api, "Delete bus", async () => {
    for (const edit of edits) await api.edit(edit);
  });
}

// ---------------------------------------------------------------------------------------------
// Sends
// ---------------------------------------------------------------------------------------------

/** `SetSends` replacing a strip's sends. */
export const setSendsCommand = (strip: MixerRef, sends: readonly AuxSend[]): EditCommand => ({
  command: "set_sends",
  target: strip,
  sends: [...sends],
});

/** A new send to `target` at 0 dB, post-fader. */
export const newSend = (target: number): AuxSend => ({
  target,
  level_db: 0,
  pre_fader: false,
});

/** The sends after adding one to bus `target`. */
export const withSend = (sends: readonly AuxSend[], target: number): AuxSend[] => [
  ...sends,
  newSend(target),
];

/** The sends with send `index` changed. */
export function changeSend(
  sends: readonly AuxSend[],
  index: number,
  change: Partial<AuxSend>,
): AuxSend[] {
  return sends.map((send, i) => (i === index ? { ...send, ...change } : send));
}

/** The sends without send `index`. */
export const withoutSend = (sends: readonly AuxSend[], index: number): AuxSend[] =>
  sends.filter((_, i) => i !== index);

/**
 * The buses a new send from `strip` could go to: the routing targets (no cycle). A bus that is
 * the strip's own output is still allowed (a send alongside the output is legitimate).
 */
export const sendTargets = routeTargets;
