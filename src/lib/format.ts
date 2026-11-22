/** "12%" for a 0..1 load; clamps and rounds so the status bar never shows noise. */
export function formatCpu(load: number): string {
  if (!Number.isFinite(load)) return "0%";
  return `${Math.round(Math.min(Math.max(load, 0), 1) * 100)}%`;
}

/** "Speakers (Realtek) - 48 kHz, 2 ch" or the failure reason. */
export function formatDevice(status: {
  running: boolean;
  deviceName: string | null;
  sampleRate: number | null;
  channels: number | null;
  error: string | null;
}): string {
  if (!status.running || !status.deviceName) {
    return status.error ? `No audio: ${status.error}` : "No audio device";
  }
  const format: string[] = [];
  if (status.sampleRate) format.push(`${+(status.sampleRate / 1000).toFixed(1)} kHz`);
  if (status.channels) format.push(`${status.channels} ch`);
  return format.length ? `${status.deviceName} - ${format.join(", ")}` : status.deviceName;
}
