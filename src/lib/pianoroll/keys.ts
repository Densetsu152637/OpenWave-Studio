import type { KeyLike } from "../arrange/keys";

/** What a key press asks of the piano roll (only while the roll has focus). */
export type RollKeyAction =
  | "delete"
  | "select-all"
  | "duplicate"
  | "deselect"
  | "nudge-left"
  | "nudge-right"
  | "nudge-up"
  | "nudge-down"
  | "octave-up"
  | "octave-down"
  | "tool-draw"
  | "tool-select"
  | "quantize"
  | "legato";

/** The roll's shortcuts, as a legend lists them. */
export const ROLL_SHORTCUTS: readonly { action: RollKeyAction; label: string; keys: string }[] = [
  { action: "tool-draw", label: "Draw tool", keys: "D" },
  { action: "tool-select", label: "Select tool", keys: "V" },
  { action: "delete", label: "Delete selected notes", keys: "Delete" },
  { action: "select-all", label: "Select all notes", keys: "Ctrl+A" },
  { action: "duplicate", label: "Duplicate selected notes", keys: "Ctrl+D" },
  { action: "deselect", label: "Clear selection", keys: "Escape" },
  { action: "nudge-left", label: "Nudge earlier by a grid step", keys: "Left" },
  { action: "nudge-right", label: "Nudge later by a grid step", keys: "Right" },
  { action: "nudge-up", label: "Transpose up a semitone", keys: "Up" },
  { action: "nudge-down", label: "Transpose down a semitone", keys: "Down" },
  { action: "octave-up", label: "Transpose up an octave", keys: "Shift+Up" },
  { action: "octave-down", label: "Transpose down an octave", keys: "Shift+Down" },
  { action: "quantize", label: "Quantize", keys: "Q" },
  { action: "legato", label: "Legato", keys: "L" },
];

/**
 * The roll action for a key press, or null. These keys are only looked at while the roll is
 * the focused area, and the roll takes them before the arrangement's own shortcuts (which turn
 * Delete, S, 0 and Ctrl+D off while a roll is open; Ctrl+A and Escape it consumes itself).
 */
export function rollKeyAction(event: KeyLike): RollKeyAction | null {
  if (event.altKey) return null;
  if (event.ctrlKey || event.metaKey) {
    if (event.shiftKey) return null;
    const letter = event.key.toLowerCase();
    if (letter === "a") return "select-all";
    return letter === "d" ? "duplicate" : null;
  }
  switch (event.key) {
    case "Delete":
    case "Backspace":
      return "delete";
    case "Escape":
      return "deselect";
    case "ArrowLeft":
      return event.shiftKey ? null : "nudge-left";
    case "ArrowRight":
      return event.shiftKey ? null : "nudge-right";
    case "ArrowUp":
      return event.shiftKey ? "octave-up" : "nudge-up";
    case "ArrowDown":
      return event.shiftKey ? "octave-down" : "nudge-down";
  }
  if (event.shiftKey) return null;
  switch (event.key.toLowerCase()) {
    case "d":
      return "tool-draw";
    case "v":
      return "tool-select";
    case "q":
      return "quantize";
    case "l":
      return "legato";
    default:
      return null;
  }
}
