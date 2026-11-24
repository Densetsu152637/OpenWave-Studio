import * as commands from "../ipc/commands";
import type { HistoryState } from "../ipc/types";

/** What recording needs from the control layer; the real one is `recordApi`. */
export interface RecordApi {
  setArm(track: number, armed: boolean): Promise<unknown>;
  start(): Promise<unknown>;
}

/** The app's control layer over the record commands. */
export const recordApi: RecordApi = {
  setArm: (track, armed) => commands.recordSetArm(track, armed),
  start: () => commands.recordStart(),
};

/** The tracks armed for recording, from the mirror's history (none before it attached). */
export function armedSet(history: Pick<HistoryState, "armedTracks"> | null | undefined): Set<number> {
  return new Set(history?.armedTracks ?? []);
}

/** Flips a track's arm state: one command with the opposite of what the mirror shows now. */
export async function toggleArm(
  api: RecordApi,
  armed: ReadonlySet<number>,
  track: number,
): Promise<boolean> {
  const next = !armed.has(track);
  await api.setArm(track, next);
  return next;
}

/** Record needs an armed track and no recording already running. */
export function canRecord(armed: ReadonlySet<number>, recording: boolean): boolean {
  return armed.size > 0 && !recording;
}

/** Starts recording; with nothing armed it rejects without asking the backend. */
export async function startRecording(api: RecordApi, armed: ReadonlySet<number>): Promise<void> {
  if (armed.size === 0) throw new Error("Arm an audio track to record on first.");
  await api.start();
}
