import { describe, expect, it } from "vitest";
import { formatCpu, formatDevice } from "./format";

describe("formatCpu", () => {
  it("rounds and clamps", () => {
    expect(formatCpu(0)).toBe("0%");
    expect(formatCpu(0.126)).toBe("13%");
    expect(formatCpu(3)).toBe("100%");
    expect(formatCpu(-1)).toBe("0%");
    expect(formatCpu(Number.NaN)).toBe("0%");
  });
});

describe("formatDevice", () => {
  const none = { running: false, deviceName: null, sampleRate: null, channels: null };

  it("shows device and format", () => {
    expect(
      formatDevice({ running: true, deviceName: "Speakers", sampleRate: 44100, channels: 2, error: null }),
    ).toBe("Speakers - 44.1 kHz, 2 ch");
  });

  it("shows the failure reason", () => {
    expect(formatDevice({ ...none, error: "boom" })).toBe("No audio: boom");
    expect(formatDevice({ ...none, error: null })).toBe("No audio device");
  });
});
