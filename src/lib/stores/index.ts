/**
 * Read-only mirrors of engine state. `modelMirror` follows the project through snapshots and
 * ordered patches; `telemetryStore` follows the 60 Hz meter and transport frames. The UI never
 * mutates either: edits go out through `$lib/ipc/commands` and come back as patches.
 */
export { ModelMirror, modelMirror } from "./model";
export { TELEMETRY_HZ, TelemetryStore, telemetryStore } from "./telemetry";
export {
  applyOps,
  applyPatch,
  detachedState,
  fromProject,
  stateFromSnapshot,
  toProject,
  type MirrorProject,
  type MirrorState,
  type PatchResult,
} from "./apply";
