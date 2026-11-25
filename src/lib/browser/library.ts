import type {
  LibraryEntryInfo,
  LibraryKey,
  LibraryQuery,
  LibraryScanStatus,
  Warp,
} from "../ipc/types";

/**
 * View-model logic of the loop browser: filters to queries, labels, scan progress and the
 * decision whether a loop is added with warp. The component only draws what this computes.
 */

const NOTE_NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/** Most bars a loop may have to be warped automatically when it is added from the browser. */
export const AUTO_WARP_MAX_BARS = 32;

/** A key as a short name: `Am`, `F#`. Empty without a key. */
export function keyLabel(key: Pick<LibraryKey, "tonic" | "minor"> | null | undefined): string {
  if (!key) return "";
  return `${NOTE_NAMES[((key.tonic % 12) + 12) % 12]}${key.minor ? "m" : ""}`;
}

/** The choices of the key filter: every key, with `""` for any. */
export const KEY_CHOICES: readonly { value: string; label: string }[] = [
  { value: "", label: "Any key" },
  ...[false, true].flatMap((minor) =>
    NOTE_NAMES.map((_, tonic) => ({
      value: `${tonic}${minor ? "m" : "M"}`,
      label: keyLabel({ tonic, minor }),
    })),
  ),
];

/** The key a filter choice names (`"9m"` is A minor); null for `""` or anything unknown. */
export function parseKeyChoice(value: string): LibraryKey | null {
  const match = /^(\d{1,2})([mM])$/.exec(value);
  if (!match) return null;
  const tonic = Number(match[1]);
  return tonic < 12 ? { tonic, minor: match[2] === "m" } : null;
}

/** What the filter row holds, as typed. */
export interface BrowserFilters {
  text: string;
  bpmMin: string;
  bpmMax: string;
  /** A value of `KEY_CHOICES`. */
  key: string;
  halfDouble: boolean;
}

export const NO_FILTERS: BrowserFilters = {
  text: "",
  bpmMin: "",
  bpmMax: "",
  key: "",
  halfDouble: false,
};

const positive = (text: string): number | null => {
  const value = Number.parseFloat(text);
  return text.trim() !== "" && Number.isFinite(value) && value > 0 ? value : null;
};

/** The search the filters ask for (an empty or invalid tempo field means no limit). */
export function toQuery(filters: BrowserFilters, limit = 500): LibraryQuery {
  return {
    text: filters.text.trim(),
    tags: [],
    bpmMin: positive(filters.bpmMin),
    bpmMax: positive(filters.bpmMax),
    includeHalfDouble: filters.halfDouble,
    key: parseKeyChoice(filters.key),
    limit,
  };
}

/** A tempo for the list: `120`, `87.5`, or a dash when the file has none. */
export function bpmLabel(bpm: number | null | undefined): string {
  if (bpm === null || bpm === undefined || !Number.isFinite(bpm)) return "–";
  const tenths = Math.round(bpm * 10);
  return tenths % 10 === 0 ? String(tenths / 10) : (tenths / 10).toFixed(1);
}

/** A length for the list: `0:08`, `2:05`. */
export function durationLabel(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "–";
  const whole = Math.round(seconds);
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, "0")}`;
}

/** 0 to 1 while a scan runs and has counted its files; null before that. */
export function scanFraction(
  status: Pick<LibraryScanStatus, "scanning" | "done" | "total">,
): number | null {
  if (!status.scanning || status.total <= 0) return null;
  return Math.min(1, status.done / status.total);
}

/** One line about the scan: progress while it runs, otherwise what the library holds. */
export function scanSummary(status: LibraryScanStatus): string {
  if (status.scanning) {
    return status.total > 0
      ? `Scanning ${status.done} of ${status.total} files…`
      : "Looking for files…";
  }
  const files = `${status.entries} file${status.entries === 1 ? "" : "s"}`;
  const failed = status.failed > 0 ? `, ${status.failed} could not be read` : "";
  return `${files}${failed}${status.cancelled ? " (scan stopped)" : ""}`;
}

/**
 * The loop length in beats and the tempo that makes it exactly that many: a loop whose
 * detected tempo is a little off (it is 7.99 beats long, say) is taken to be the whole beats it
 * is nearly, so it fills bars exactly when it is stretched. Within a tenth of a beat counts.
 */
export function loopBeats(seconds: number, bpm: number): { beats: number; bpm: number } {
  const raw = (seconds * bpm) / 60;
  const whole = Math.round(raw);
  if (whole >= 1 && Math.abs(raw - whole) <= 0.1) {
    return { beats: whole, bpm: (whole * 60) / seconds };
  }
  return { beats: raw, bpm };
}

/**
 * The warp a loop gets when it is added from the browser: follow the project tempo at the
 * loop's own tempo, unless the tempo is unknown, the file is not a loop (more than 32 bars, or
 * under a beat) or it has no length. Null leaves the clip as it is.
 */
export function autoWarp(bpm: number | null | undefined, seconds: number): Warp | null {
  if (bpm === null || bpm === undefined || !Number.isFinite(bpm) || bpm <= 0) return null;
  if (!Number.isFinite(seconds) || seconds <= 0) return null;
  const { beats, bpm: exact } = loopBeats(seconds, bpm);
  if (beats < 1 || beats / 4 > AUTO_WARP_MAX_BARS) return null;
  return { mode: "follow_tempo", source_bpm: exact, markers: [], transpose_semitones: 0 };
}

/** What to show when the list is empty. */
export function emptyHint(total: number, scanning: boolean, folders: number): string {
  if (scanning) return "Scanning…";
  if (total === 0) {
    return folders === 0
      ? "No library folders yet. Add some in Settings, Folders, then scan."
      : "The library is empty. Scan to read the folders.";
  }
  return "No file matches.";
}

/** A search result row's tooltip: the path and the tags. */
export function entryTitle(entry: LibraryEntryInfo): string {
  return entry.tags.length > 0 ? `${entry.path}\n${entry.tags.join(", ")}` : entry.path;
}
