/** Sizing of the bottom dock (piano roll, FX, sends, device panel). Pure so it can be tested. */

/** Dock height when no roll is open. */
export const DOCK_PLAIN_HEIGHT = 280;
/** Smallest dock without a roll. */
export const DOCK_PLAIN_MIN = 120;
/** Height the roll needs besides its note grid: toolbar rows plus the velocity and CC lanes. */
export const ROLL_CHROME = 300;
/** Note grid height the minimum dock keeps. */
export const MIN_GRID = 160;
/** Arrangement height the maximum dock leaves. */
export const MIN_ARRANGEMENT = 120;
/** Default roll dock height: this fraction of the available height, but at least ROLL_DEFAULT_MIN. */
export const ROLL_DEFAULT_FRACTION = 0.45;
export const ROLL_DEFAULT_MIN = 420;
export const DOCK_KEY = "openwave.dock.height";

/** Smallest dock for the current content. */
export function dockMin(rollOpen: boolean): number {
  return rollOpen ? MIN_GRID + ROLL_CHROME : DOCK_PLAIN_MIN;
}

/** Largest dock in a column of `available` px; never below the minimum, so tiny windows still work. */
export function dockMax(available: number, rollOpen: boolean): number {
  return Math.max(dockMin(rollOpen), Math.floor(available - MIN_ARRANGEMENT));
}

/** Clamps a requested height to what the content and the available space allow. */
export function clampDock(height: number, available: number, rollOpen: boolean): number {
  const max = dockMax(available, rollOpen);
  const min = Math.min(dockMin(rollOpen), max);
  if (!Number.isFinite(height)) return min;
  return Math.round(Math.min(max, Math.max(min, height)));
}

/** Height used until the user drags the handle. */
export function defaultDock(available: number, rollOpen: boolean): number {
  const wanted = rollOpen
    ? Math.max(ROLL_DEFAULT_MIN, Math.round(available * ROLL_DEFAULT_FRACTION))
    : DOCK_PLAIN_HEIGHT;
  return clampDock(wanted, available, rollOpen);
}

/** The height to show: the user's one when set, else the default, always clamped. */
export function dockHeight(saved: number | null, available: number, rollOpen: boolean): number {
  return saved === null ? defaultDock(available, rollOpen) : clampDock(saved, available, rollOpen);
}

/** Reads a stored height; anything that is not a positive finite number means "use the default". */
export function parseDockHeight(raw: string | null): number | null {
  if (raw === null) return null;
  const value = Number(raw);
  return Number.isFinite(value) && value > 0 ? Math.round(value) : null;
}
