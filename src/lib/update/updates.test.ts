import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  updateInstallNow: vi.fn(),
  updateSetChannel: vi.fn(),
  updateRestart: vi.fn(),
}));

vi.mock("./index", async (original) => ({ ...(await original<typeof import("./index")>()), ...api }));

const { updates } = await import("./updates.svelte");
const { offlineStatus } = await import("./index");

describe("updates store", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    updates.status = offlineStatus;
    updates.error = null;
    updates.installing = false;
  });

  it("is busy while a check-and-install runs and takes the returned status", async () => {
    let finish: (value: unknown) => void = () => {};
    api.updateInstallNow.mockReturnValue(new Promise((resolve) => (finish = resolve)));
    const run = updates.installNow();
    expect(updates.installing).toBe(true);
    expect(updates.busy).toBe(true);
    finish({ ...offlineStatus, state: "up_to_date" });
    await run;
    expect(updates.installing).toBe(false);
    expect(updates.status.state).toBe("up_to_date");
  });

  it("shows why nothing was installed", async () => {
    api.updateInstallNow.mockRejectedValue("Checking for updates failed: offline");
    await updates.installNow();
    expect(updates.installing).toBe(false);
    expect(updates.error).toBe("Checking for updates failed: offline");
  });

  it("switches channel and clears an earlier error", async () => {
    updates.error = "old";
    api.updateSetChannel.mockResolvedValue({ ...offlineStatus, channel: "experimental" });
    await updates.setChannel("experimental");
    expect(api.updateSetChannel).toHaveBeenCalledWith("experimental");
    expect(updates.status.channel).toBe("experimental");
    expect(updates.error).toBeNull();
  });

  it("is busy while the backend checks or downloads", () => {
    updates.status = { ...offlineStatus, state: "downloading", version: "1.0.0" };
    expect(updates.busy).toBe(true);
    updates.status = { ...offlineStatus, state: "ready", version: "1.0.0", notes: null };
    expect(updates.busy).toBe(false);
  });
});
