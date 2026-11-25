/** What an arrangement key press asks for. */
export type ArrangeKeyAction =
  | "locate-start"
  | "locate-end"
  | "zoom-in"
  | "zoom-out"
  | "add-marker"
  | "select-all"
  | "deselect"
  | "delete-selection";

export interface KeyLike {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

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
];

/**
 * The arrangement action for a key press, or null. Ctrl/Cmd+A is the only chord; the rest need no
 * Ctrl, Cmd or Alt, so they never shadow the app's own shortcuts. Callers ignore presses while
 * focus is in a text field.
 */
export function arrangeKeyAction(event: KeyLike): ArrangeKeyAction | null {
  if (event.ctrlKey || event.metaKey) {
    return !event.altKey && !event.shiftKey && event.key.toLowerCase() === "a"
      ? "select-all"
      : null;
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
    case "Delete":
    case "Backspace":
      return "delete-selection";
    default:
      return null;
  }
}
