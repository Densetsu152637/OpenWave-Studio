import { beforeEach, describe, expect, it, vi } from "vitest";
import { loadDescriptors, resetDescriptors } from "./descriptors";
import {
  clampParam,
  descriptorFor,
  formatParam,
  newDevice,
  panelControls,
  panelGroups,
  paramValue,
  positionToValue,
  setParamCommand,
  shortName,
  valueToPosition,
} from "./params";
import type { DeviceDescriptor, ParamDescriptorDto } from "../ipc/types";

const param = (over: Partial<ParamDescriptorDto>): ParamDescriptorDto => ({
  id: 1,
  name: "P",
  min: 0,
  max: 1,
  default: 0.5,
  unit: "none",
  scale: "linear",
  choices: [],
  ...over,
});

const waveform = param({ id: 0, name: "Waveform", max: 2, default: 0, choices: ["Saw", "Square", "Sine"] });
const cutoff = param({ id: 1, name: "Cutoff", min: 20, max: 20_000, default: 8000, unit: "hertz", scale: "log" });
const sustain = param({ id: 5, name: "Sustain", default: 0.7 });
const release = param({ id: 6, name: "Release", min: 0.001, max: 10, default: 0.3, unit: "seconds", scale: "log" });
const gain = param({ id: 7, name: "Gain", min: -60, max: 6, default: -9, unit: "decibels" });

const synth: DeviceDescriptor = {
  kind: "builtin.synth",
  name: "Synth",
  instrument: true,
  params: [waveform, cutoff, sustain, release, gain],
};

describe("parameter values", () => {
  it("uses the stored value, else the default", () => {
    expect(paramValue({ params: { 1: 440 } }, cutoff)).toBe(440);
    expect(paramValue({ params: {} }, cutoff)).toBe(8000);
  });

  it("clamps to the range and rounds choices", () => {
    expect(clampParam(cutoff, 5)).toBe(20);
    expect(clampParam(cutoff, 1e9)).toBe(20_000);
    expect(clampParam(cutoff, Number.NaN)).toBe(8000);
    expect(clampParam(waveform, 1.4)).toBe(1);
    expect(clampParam(waveform, 7)).toBe(2);
    // A stored value outside the range (an edited file) reads back clamped.
    expect(paramValue({ params: { 5: 3 } }, sustain)).toBe(1);
  });
});

describe("control travel", () => {
  it("maps linear parameters evenly", () => {
    expect(valueToPosition(gain, -60)).toBe(0);
    expect(valueToPosition(gain, 6)).toBe(1);
    expect(valueToPosition(gain, -27)).toBeCloseTo(0.5);
    expect(positionToValue(gain, 0.5)).toBeCloseTo(-27);
  });

  it("maps log parameters by octaves, so the middle is the geometric mean", () => {
    expect(valueToPosition(cutoff, 20)).toBeCloseTo(0);
    expect(valueToPosition(cutoff, 20_000)).toBeCloseTo(1);
    expect(positionToValue(cutoff, 0.5)).toBeCloseTo(Math.sqrt(20 * 20_000), 3);
    for (const v of [20, 100, 1000, 8000, 20_000]) {
      expect(positionToValue(cutoff, valueToPosition(cutoff, v))).toBeCloseTo(v, 6);
    }
  });

  it("treats choices as evenly spaced indices", () => {
    expect(positionToValue(waveform, 0)).toBe(0);
    expect(positionToValue(waveform, 0.49)).toBe(1);
    expect(positionToValue(waveform, 1)).toBe(2);
    expect(valueToPosition(waveform, 1)).toBe(0.5);
  });

  it("is safe against nonsense", () => {
    expect(positionToValue(cutoff, -3)).toBe(20);
    expect(positionToValue(cutoff, 3)).toBe(20_000);
    expect(positionToValue(cutoff, Number.NaN)).toBe(20);
    expect(valueToPosition(param({ min: 1, max: 1 }), 1)).toBe(0);
  });
});

describe("value text", () => {
  it("formats by unit", () => {
    expect(formatParam(cutoff, 440)).toBe("440 Hz");
    expect(formatParam(cutoff, 8000)).toBe("8 kHz");
    expect(formatParam(cutoff, 12_500)).toBe("12.5 kHz");
    expect(formatParam(release, 0.3)).toBe("300 ms");
    expect(formatParam(release, 0.0015)).toBe("1.5 ms");
    expect(formatParam(release, 2.5)).toBe("2.5 s");
    expect(formatParam(gain, -9)).toBe("-9 dB");
    expect(formatParam(sustain, 0.7)).toBe("0.7");
    expect(formatParam(waveform, 2)).toBe("Sine");
  });
});

describe("edits", () => {
  it("sets a parameter with SetDeviceParam", () => {
    expect(setParamCommand({ id: 12 }, cutoff, 440)).toEqual({
      command: "set_device_param",
      device: 12,
      param: 1,
      value: 440,
    });
  });

  it("clears the stored value when it is set back to the default", () => {
    expect(setParamCommand({ id: 12 }, cutoff, 8000)).toMatchObject({ value: null });
    // Out-of-range input is clamped first.
    expect(setParamCommand({ id: 12 }, gain, 100)).toMatchObject({ value: 6 });
  });
});

describe("generated panel", () => {
  it("lists a control per descriptor parameter, in order", () => {
    const device = { ...newDevice(synth, 3), params: { 0: 2, 1: 1000 } };
    const controls = panelControls(device, synth);
    expect(controls.map((c) => c.param.name)).toEqual(["Waveform", "Cutoff", "Sustain", "Release", "Gain"]);
    expect(controls.map((c) => c.choice)).toEqual([true, false, false, false, false]);
    expect(controls[0]).toMatchObject({ value: 2, text: "Sine", position: 1 });
    expect(controls[1]).toMatchObject({ value: 1000, text: "1 kHz" });
    expect(controls[2]).toMatchObject({ value: 0.7, text: "0.7" });
  });

  it("builds a new device at its defaults and finds descriptors by kind", () => {
    const device = newDevice(synth, 9);
    expect(device).toEqual({
      id: 9,
      kind: "builtin.synth",
      name: "Synth",
      enabled: true,
      params: {},
      state: [],
      sidechain: null,
    });
    expect(descriptorFor([synth], device)).toBe(synth);
    expect(descriptorFor([synth], { kind: "vst3:x" })).toBeUndefined();
  });
});

describe("sections", () => {
  const osc2Level = param({ id: 13, name: "Osc 2 Level", group: "Osc 2" });
  const osc2Wave = param({ id: 12, name: "Osc 2 Waveform", group: "Osc 2", max: 3, default: 0, choices: ["Saw", "Square", "Sine", "Tri"] });
  const osc1Wave = param({ id: 0, name: "Waveform", group: "Osc 1", max: 1, default: 0, choices: ["A", "B"] });
  const envAmount = param({ id: 23, name: "Filter Env Amount", group: "Filter", min: -96, max: 96, default: 0, unit: "semitones" });
  const filterAttack = param({ id: 24, name: "Filter Attack", group: "Filter Envelope", min: 0.001, max: 5, default: 0.01, unit: "seconds" });
  const loose = param({ id: 99, name: "Loose" });
  const modular: DeviceDescriptor = {
    kind: "builtin.x",
    name: "X",
    instrument: true,
    params: [osc1Wave, osc2Level, loose, osc2Wave, envAmount, filterAttack],
  };

  it("groups controls by section in order of first appearance", () => {
    const groups = panelGroups({ params: {} }, modular);
    expect(groups.map((g) => g.name)).toEqual(["Osc 1", "Osc 2", "", "Filter", "Filter Envelope"]);
    expect(groups[1].controls.map((c) => c.param.name)).toEqual(["Osc 2 Level", "Osc 2 Waveform"]);
  });

  it("shortens names that repeat their section and keeps the rest", () => {
    expect(shortName(osc2Level)).toBe("Level");
    expect(shortName(envAmount)).toBe("Env Amount");
    expect(shortName(filterAttack)).toBe("Filter Attack");
    expect(shortName(osc1Wave)).toBe("Waveform");
    expect(shortName(param({ name: "Op 1", group: "Op 1" }))).toBe("Op 1");
    expect(shortName(loose)).toBe("Loose");
  });

  it("marks short choice lists as segmented", () => {
    const controls = panelControls({ params: {} }, modular);
    expect(controls.map((c) => c.segmented)).toEqual([true, false, false, true, false, false]);
    const long = param({ max: 4, choices: ["a", "b", "c", "d", "e"] });
    expect(panelControls({ params: {} }, { params: [long] })[0].segmented).toBe(false);
  });

  it("formats pitch units", () => {
    expect(formatParam(envAmount, 12)).toBe("+12 st");
    expect(formatParam(envAmount, -3.25)).toBe("-3.3 st");
    expect(formatParam(param({ unit: "cents", min: -100, max: 100, default: 0 }), 7)).toBe("+7 ct");
    expect(formatParam(envAmount, 0)).toBe("0 st");
  });
});

describe("descriptor loading", () => {
  beforeEach(resetDescriptors);

  it("fetches once and shares the result", async () => {
    const fetch = vi.fn(async () => [synth]);
    const [a, b] = await Promise.all([loadDescriptors(fetch), loadDescriptors(fetch)]);
    expect(a).toBe(b);
    await loadDescriptors(fetch);
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("tries again after a failure", async () => {
    const fetch = vi.fn().mockRejectedValueOnce(new Error("no backend")).mockResolvedValue([synth]);
    await expect(loadDescriptors(fetch)).rejects.toThrow("no backend");
    await expect(loadDescriptors(fetch)).resolves.toEqual([synth]);
    expect(fetch).toHaveBeenCalledTimes(2);
  });
});
