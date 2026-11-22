import { describe, expect, it } from "vitest";
import { canCheck, describeStatus, offlineStatus, type UpdateStatus } from "./index";

const base = { currentVersion: "0.1.9-experimental", channel: "experimental", automatic: true } as const;

describe("describeStatus", () => {
  it("names the ready version", () => {
    const status: UpdateStatus = { ...base, state: "ready", version: "0.1.10-experimental", notes: null };
    expect(describeStatus(status)).toContain("0.1.10-experimental");
  });

  it("shows the package manager message for a disabled install", () => {
    const status: UpdateStatus = {
      ...base,
      state: "disabled",
      reason: "package_managed",
      message: "Update OpenWave Studio through your package manager",
    };
    expect(describeStatus(status)).toBe("Update OpenWave Studio through your package manager");
  });

  it("reports failures verbatim", () => {
    expect(describeStatus({ ...base, state: "failed", message: "offline" })).toBe("offline");
  });
});

describe("canCheck", () => {
  it("is off while busy or disabled", () => {
    expect(canCheck({ ...base, state: "checking" })).toBe(false);
    expect(canCheck({ ...base, state: "downloading", version: "1" })).toBe(false);
    expect(canCheck({ ...base, state: "disabled", reason: "portable", message: "x" })).toBe(false);
  });

  it("is on when idle or finished", () => {
    expect(canCheck(offlineStatus)).toBe(true);
    expect(canCheck({ ...base, state: "up_to_date" })).toBe(true);
  });
});
