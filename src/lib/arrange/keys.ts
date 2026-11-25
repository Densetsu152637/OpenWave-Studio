/** What an arrangement key press asks for. */
export type ArrangeKeyAction =
  | "locate-start"
  | "locate-end"
  | "zoom-in"
  | "zoom-out"
  | "add-marker"
  | "select-all"
  | "deselect"
  | "delete-selection"
  | "split"
  | "duplicate"
  | "toggle-mute";

export interface KeyLike {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  /** Auto-repeat of a held key. */
  repeat?: boolean;
}

/** Actions that happen once per press: a held key must not repeat them. */
const SINGLE_SHOT: ReadonlySet<ArrangeKeyAction> = new Set([
  "add-marker",
  "delete-selection",
  "split",
  "duplicate",
  "toggle-mute",
]);

/** The arrangement shortcuts as the Settings panel lists them. */
export const ARRANGE_SHORTCUTS: readonly {
  id: string;
  action: ArrangeKeyAction;
  label: string;
  default: string;
}[] = [
  { id: "arrange.locate-start", action: "locate-start", label: "Go to project start", default: "Home" },
  { id: "arrange.locate-end", action: "locate-end", label: "Go to project end", default: "End" },
  { id: "arrange.zoom-in", action: "zoom-in", label: "Zoom in", default: "+" },
  { id: "arrange.zoom-out", action: "zoom-out", label: "Zoom out", default: "-" },
  { id: "arrange.add-marker", action: "add-marker", label: "Add marker at playhead", default: "M" },
  { id: "arrange.select-all", action: "select-all", label: "Select all clips", default: "Ctrl+A" },
  { id: "arrange.deselect", action: "deselect", label: "Clear selection", default: "Escape" },
  {
    id: "arrange.delete-selection",
    action: "delete-selection",
    label: "Delete selected clips",
    default: "Delete",
  },
  { id: "arrange.split", action: "split", label: "Split clips at playhead", default: "S" },
  { id: "arrange.duplicate", action: "duplicate", label: "Duplicate selected clips", default: "Ctrl+D" },
  { id: "arrange.toggle-mute", action: "toggle-mute", label: "Mute or unmute selected clips", default: "0" },
];

/**
 * The arrangement action for a key press, or null. Ctrl/Cmd+A and Ctrl/Cmd+D are the only chords; the rest need no
 * Ctrl, Cmd or Alt, so they never shadow the app's own shortcuts. Callers ignore presses while
 * focus is in a text field. `rollOpen` (the piano roll is showing) turns Delete, Backspace, S, 0 and Ctrl/Cmd+D off.
 */
export function arrangeKeyAction(
  event: KeyLike,
  context: { rollOpen?: boolean } = {},
): ArrangeKeyAction | null {
  const action = keyAction(event, context);
  return action && event.repeat && SINGLE_SHOT.has(action) ? null : action;
}

function keyAction(event: KeyLike, context: { rollOpen?: boolean }): ArrangeKeyAction | null {
  if (event.ctrlKey || event.metaKey) {
    if (event.altKey || event.shiftKey) return null;
    const letter = event.key.toLowerCase();
    if (letter === "a") return "select-all";
    // Duplicating clips would fight the piano roll's own selection, so the roll gets the key.
    return letter === "d" && !context.rollOpen ? "duplicate" : null;
  }
  if (event.altKey) return null;
  switch (event.key) {
    case "Home":
      return "locate-start";
    case "End":
      return "locate-end";
    case "+":
    case "=":
      return "zoom-in";
    case "-":
    case "_":
      return "zoom-out";
    case "m":
    case "M":
      return "add-marker";
    case "Escape":
      return "deselect";
    // Clip editing keys belong to the piano roll while it is open.
    case "s":
    case "S":
      return context.rollOpen ? null : "split";
    case "0":
      return context.rollOpen ? null : "toggle-mute";
    case "Delete":
    case "Backspace":
      // With the piano roll open these keys belong to it: they must never delete its clip.
      return context.rollOpen ? null : "delete-selection";
    default:
      return null;
  }
}
