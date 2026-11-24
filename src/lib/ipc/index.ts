/**
 * The UI's side of the IPC contract. `engine.ts` holds the engine status and preferences wrappers
 * (formerly `src/lib/ipc.ts`; the exports are unchanged); the rest is the project bridge:
 * generated DTO types, typed commands, the telemetry frame decoder and the peak-range decoder.
 */
export * from "./engine";
export * from "./commands";
export * from "./peaks";
export * from "./telemetry";
export * from "./transport";
export type * from "./types";
