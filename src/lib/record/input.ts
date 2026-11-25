import type { EditCommand, InputChannels, MonitorMode, Track, TrackInput } from "../ipc/types";

/** Widest input device the selector lists (mono inputs 1..N). */
export const MAX_INPUT_CHANNELS = 64;

export interface InputOption {
  /** Stable select value: "default", "m3" (mono, channel 3) or "s1" (stereo, channels 1-2). */
  id: string;
  label: string;
  /** What the track's input becomes; null is the default. */
  input: TrackInput | null;
}

const DEFAULT_OPTION: InputOption = { id: "default", label: "Default", input: null };

/** Channels count from 0 in the model and from 1 on screen. */
export function channelsLabel(channels: InputChannels): string {
  return "mono" in channels
    ? `Mono ${channels.mono + 1}`
    : `Stereo ${channels.stereo + 1}-${channels.stereo + 2}`;
}

export function inputId(input: TrackInput | null): string {
  if (!input) return "default";
  return "mono" in input.channels ? `m${input.channels.mono}` : `s${input.channels.stereo}`;
}

/** Whether `channels` fits an input device with `deviceChannels` channels. */
export function fitsDevice(channels: InputChannels, deviceChannels: number): boolean {
  return "mono" in channels
    ? channels.mono < deviceChannels
    : channels.stereo + 2 <= deviceChannels;
}

/**
 * The choices for an input device with `deviceChannels` channels (null or 0: no input device):
 * Default, Mono 1..N, then the adjacent pairs Stereo 1-2, 3-4... A pair never straddles an odd
 * leftover channel. The track's `current` input is kept as a (marked) choice when the device
 * has no such channels, so the selector never shows a value that is not there.
 */
export function inputOptions(
  deviceChannels: number | null,
  current: TrackInput | null = null,
): InputOption[] {
  const count = Math.min(MAX_INPUT_CHANNELS, Math.max(0, Math.floor(deviceChannels ?? 0)));
  const options: InputOption[] = [DEFAULT_OPTION];
  for (let ch = 0; ch < count; ch++) {
    options.push({ id: `m${ch}`, label: `Mono ${ch + 1}`, input: { channels: { mono: ch } } });
  }
  for (let ch = 0; ch + 2 <= count; ch += 2) {
    options.push({
      id: `s${ch}`,
      label: `Stereo ${ch + 1}-${ch + 2}`,
      input: { channels: { stereo: ch } },
    });
  }
  if (current && !options.some((o) => o.id === inputId(current))) {
    options.push({
      id: inputId(current),
      label: `${channelsLabel(current.channels)} (not on this device)`,
      input: current,
    });
  }
  return options;
}

/** The option with `id`, from the list the selector shows. */
export function optionById(options: readonly InputOption[], id: string): InputOption | undefined {
  return options.find((o) => o.id === id);
}

export const setTrackInputCommand = (
  track: Pick<Track, "id">,
  input: TrackInput | null,
): EditCommand => ({ command: "set_track_input", track: track.id, input });

/** Monitoring cycles Off, Auto (heard while armed), On (always heard). */
const NEXT_MONITOR: Record<MonitorMode, MonitorMode> = { off: "auto", auto: "on", on: "off" };

export const nextMonitor = (mode: MonitorMode): MonitorMode => NEXT_MONITOR[mode];

export interface MonitorView {
  /** Short text for the compact button. */
  label: string;
  title: string;
  /** Whether the button looks lit. */
  lit: boolean;
}

export function monitorView(mode: MonitorMode): MonitorView {
  switch (mode) {
    case "off":
      return { label: "Mon", title: "Input monitoring: off (click for auto)", lit: false };
    case "auto":
      return {
        label: "Mon A",
        title: "Input monitoring: auto, heard while the track is armed (click for on)",
        lit: true,
      };
    case "on":
      return {
        label: "Mon On",
        title: "Input monitoring: always on (click for off)",
        lit: true,
      };
  }
}

export const setTrackMonitorCommand = (
  track: Pick<Track, "id">,
  monitor: MonitorMode,
): EditCommand => ({ command: "set_track_monitor", track: track.id, monitor });
