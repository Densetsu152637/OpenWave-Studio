import { describe, expect, it } from "vitest";
import type { DeviceDescriptor } from "../ipc/types";
import {
  deviceLabel,
  instrumentDescriptors,
  loadDescriptors,
  pluginDescriptors,
  resetDescriptors,
  watchPlugins,
} from "./descriptors";

const builtin: DeviceDescriptor = { kind: "builtin.synth", name: "Synth", instrument: true, params: [] };
const effect: DeviceDescriptor = { kind: "builtin.eq", name: "EQ", instrument: false, params: [] };
const plugin: DeviceDescriptor = {
  kind: "clap:com.vendor.pad",
  name: "Pad",
  instrument: true,
  params: [],
  format: "clap",
  vendor: "Vendor",
};

describe("device descriptors", () => {
  it("tells instruments, plugins and menu labels apart", () => {
    const all = [builtin, effect, plugin];
    expect(instrumentDescriptors(all).map((d) => d.kind)).toEqual(["builtin.synth", "clap:com.vendor.pad"]);
    expect(pluginDescriptors(all)).toEqual([plugin]);
    expect(deviceLabel(plugin)).toBe("Pad (Vendor)");
    expect(deviceLabel(effect)).toBe("EQ");
    expect(deviceLabel({ name: "Anon", vendor: "" })).toBe("Anon");
  });

  it("fetches once and again after a reset, and forgets a failed fetch", async () => {
    resetDescriptors();
    let calls = 0;
    const fetch = async () => {
      calls += 1;
      return [builtin];
    };
    await loadDescriptors(fetch);
    await loadDescriptors(fetch);
    expect(calls).toBe(1);
    resetDescriptors();
    await loadDescriptors(fetch);
    expect(calls).toBe(2);

    resetDescriptors();
    await expect(loadDescriptors(async () => Promise.reject(new Error("down")))).rejects.toThrow("down");
    await expect(loadDescriptors(fetch)).resolves.toEqual([builtin]);
    resetDescriptors();
  });

  it("listens for plugin changes only inside the app", async () => {
    // A plain test environment has no Tauri event system: a no-op, not an error.
    const stop = await watchPlugins(() => undefined);
    expect(() => stop()).not.toThrow();
  });
});
