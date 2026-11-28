/** Sizing of the bottom dock (piano roll, FX, sends, device panel). Pure so it can be tested. */

/** Dock height when no roll is open. */
export const DOCK_PLAIN_HEIGHT = 280;
/** Smallest dock without a roll, preferred and absolute. */
export const DOCK_PLAIN_MIN = 120;
export const DOCK_FLOOR = 80;
/** Note grid height the preferred minimum keeps, and the least it may shrink to when space is short. */
export const MIN_GRID = 160;
export const FLOOR_GRID = 60;
/** Height kept for about one track lane above the dock, besides the measured toolbar and ruler. */
export const ONE_LANE = 100;
/** Default roll dock height: this fraction of the available height, but at least ROLL_DEFAULT_MIN. */
export const ROLL_DEFAULT_FRACTION = 0.45;
export const ROLL_DEFAULT_MIN = 420;
export const DOCK_KEY = "openwave.dock.roll.height";

/** What the dock is sized against; all in px and measured by the caller. */
export interface DockSpace {
  /** Height of the arrangement column. */
  available: number;
  /** Height the rest of the column needs: toolbar, ruler rows and about one lane. */
  reserve: number;
  /** Height of the roll besides its note grid: toolbar rows and the velocity and CC lanes. */
  chrome: number;
}

/** The reserve for a measured toolbar and ruler block. */
export function reserveOf(toolbar: number, ruler: number): number {
  return toolbar + ruler + ONE_LANE;
}

/** Preferred smallest dock for the content. */
export function dockMin(rollOpen: boolean, chrome: number): number {
  return rollOpen ? chrome + MIN_GRID : DOCK_PLAIN_MIN;
}

/** Smallest dock when space is short. */
export function dockFloor(rollOpen: boolean, chrome: number): number {
  return rollOpen ? chrome + FLOOR_GRID : DOCK_FLOOR;
}

/** Largest dock: what the column can spare, but never below the floor. */
export function dockMax(space: DockSpace, rollOpen: boolean): number {
  return Math.max(dockFloor(rollOpen, space.chrome), Math.floor(space.available - space.reserve));
}

/** The smallest dock that is allowed now: the preferred minimum, shrunk when the space is short. */
export function dockLowest(space: DockSpace, rollOpen: boolean): number {
  return Math.min(dockMin(rollOpen, space.chrome), dockMax(space, rollOpen));
}

/** Clamps a requested height to what the content and the available space allow. */
export function clampDock(height: number, space: DockSpace, rollOpen: boolean): number {
  const min = dockLowest(space, rollOpen);
  if (!Number.isFinite(height)) return min;
  return Math.round(Math.min(dockMax(space, rollOpen), Math.max(min, height)));
}

/** Height used until the user drags the handle. */
export function defaultDock(space: DockSpace, rollOpen: boolean): number {
  const wanted = rollOpen
    ? Math.max(ROLL_DEFAULT_MIN, Math.round(space.available * ROLL_DEFAULT_FRACTION))
    : DOCK_PLAIN_HEIGHT;
  return clampDock(wanted, space, rollOpen);
}

/** The height to show: the user's one for a roll, else the default, always clamped. */
export function dockHeight(saved: number | null, space: DockSpace, rollOpen: boolean): number {
  return rollOpen && saved !== null ? clampDock(saved, space, true) : defaultDock(space, rollOpen);
}

/** Reads a stored height; anything that is not a positive finite number means "use the default". */
export function parseDockHeight(raw: string | null): number | null {
  if (raw === null) return null;
  const value = Number(raw);
  return Number.isFinite(value) && value > 0 ? Math.round(value) : null;
}
