/** A musical scale as the semitone steps above its root. */
export interface Scale {
  id: string;
  label: string;
  steps: readonly number[];
}

/** The scales the roll can highlight (the first, "Off", highlights nothing). */
export const SCALES: readonly Scale[] = [
  { id: "off", label: "Off", steps: [] },
  { id: "major", label: "Major", steps: [0, 2, 4, 5, 7, 9, 11] },
  { id: "minor", label: "Minor", steps: [0, 2, 3, 5, 7, 8, 10] },
  { id: "dorian", label: "Dorian", steps: [0, 2, 3, 5, 7, 9, 10] },
  { id: "mixolydian", label: "Mixolydian", steps: [0, 2, 4, 5, 7, 9, 10] },
  { id: "pentatonic-major", label: "Pentatonic major", steps: [0, 2, 4, 7, 9] },
  { id: "pentatonic-minor", label: "Pentatonic minor", steps: [0, 3, 5, 7, 10] },
];

/** The twelve root choices, C first (the root is a pitch class, 0 to 11). */
export const ROOTS: readonly string[] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/** How a key row is shaded: the scale's root, another scale tone, or outside the scale. */
export type RowKind = "root" | "scale" | "out" | "plain";

/** The scale with `id` (Off when unknown). */
export const scaleById = (id: string): Scale => SCALES.find((s) => s.id === id) ?? SCALES[0]!;

const degree = (key: number, root: number) => (((key - root) % 12) + 12) % 12;

/** True when `key` is a tone of `scale` rooted at pitch class `root`. */
export const inScale = (key: number, root: number, scale: Scale): boolean =>
  scale.steps.includes(degree(key, root));

/** How the row of `key` is shaded; "plain" for every row when the scale is Off. */
export function rowKind(key: number, root: number, scale: Scale): RowKind {
  if (scale.steps.length === 0) return "plain";
  if (degree(key, root) === 0) return "root";
  return inScale(key, root, scale) ? "scale" : "out";
}
