import type { DeviceLists, Preferences } from "../ipc/engine";

/**
 * How many channels the input device the app records from has, from the device lists and the
 * saved audio preferences; null when there is no input device. A saved device that is gone, or
 * none chosen, means the system default input.
 */
export function inputChannelCount(
  devices: DeviceLists,
  audio: Pick<Preferences["audio"], "backend" | "inputDevice">,
): number | null {
  const backends = devices.backends.filter((b) => audio.backend === "" || b.id === audio.backend);
  const pool = (backends.length > 0 ? backends : devices.backends).flatMap((b) => b.devices);
  const chosen =
    audio.inputDevice !== null && audio.inputDevice !== devices.defaultDevice
      ? pool.find((d) => d.id === audio.inputDevice && d.maxInputChannels > 0)
      : undefined;
  const device = chosen ?? pool.find((d) => d.isDefaultInput && d.maxInputChannels > 0);
  return device ? device.maxInputChannels : null;
}
