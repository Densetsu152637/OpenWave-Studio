import type {
  Device,
  DeviceDescriptor,
  EditCommand,
  ParamDescriptorDto,
} from "../ipc/types";

/**
 * View-model of the generic device panel: everything a knob, slider or selector needs, derived
 * from a device's descriptor. A panel is generated from `DeviceDescriptor.params`, so a new
 * built-in device needs no UI code of its own. Values live in the project (`Device.params`);
 * nothing here keeps state.
 */

/** The value of a parameter in `device`: the project's value, or the descriptor's default. */
export function paramValue(device: Pick<Device, "params">, param: ParamDescriptorDto): number {
  const stored = device.params[param.id];
  return stored === undefined ? param.default : clampParam(param, stored);
}

/** True for a parameter that picks one of a list of options. */
export const isChoice = (param: ParamDescriptorDto): boolean => param.choices.length > 0;

/** `value` limited to the parameter's range; a choice also rounds to a whole index. */
export function clampParam(param: ParamDescriptorDto, value: number): number {
  if (!Number.isFinite(value)) return param.default;
  const limited = Math.min(param.max, Math.max(param.min, value));
  return isChoice(param) ? Math.round(limited) : limited;
}

/** Where `value` sits on a control's travel, 0 (minimum) to 1 (maximum). */
export function valueToPosition(param: ParamDescriptorDto, value: number): number {
  const span = param.max - param.min;
  if (!(span > 0)) return 0;
  const v = clampParam(param, value);
  if (param.scale === "log" && param.min > 0) {
    return Math.log(v / param.min) / Math.log(param.max / param.min);
  }
  return (v - param.min) / span;
}

/** The value at position `position` (0 to 1) of a control's travel. */
export function positionToValue(param: ParamDescriptorDto, position: number): number {
  const t = Math.min(1, Math.max(0, Number.isFinite(position) ? position : 0));
  const value =
    param.scale === "log" && param.min > 0
      ? param.min * Math.pow(param.max / param.min, t)
      : param.min + t * (param.max - param.min);
  return clampParam(param, value);
}

/** A value as the panel shows it: with its unit, or the option's label for a choice. */
export function formatParam(param: ParamDescriptorDto, value: number): string {
  const v = clampParam(param, value);
  if (isChoice(param)) return param.choices[v] ?? String(v);
  switch (param.unit) {
    case "hertz":
      return v >= 1000 ? `${trim(v / 1000, v >= 10_000 ? 1 : 2)} kHz` : `${trim(v, v >= 100 ? 0 : 1)} Hz`;
    case "seconds":
      return v < 1 ? `${trim(v * 1000, v < 0.01 ? 1 : 0)} ms` : `${trim(v, 2)} s`;
    case "decibels":
      return `${trim(v, 1)} dB`;
    case "percent":
      return `${trim(v, 0)} %`;
    case "semitones":
      return `${v > 0 ? "+" : ""}${trim(v, 1)} st`;
    case "cents":
      return `${v > 0 ? "+" : ""}${trim(v, 0)} ct`;
    default:
      return trim(v, 2);
  }
}

/** `value` with at most `digits` decimals and no trailing zeros. */
function trim(value: number, digits: number): string {
  return String(Number(value.toFixed(digits)));
}

/**
 * The edit that sets one parameter: `SetDeviceParam`. A value equal to the default clears the
 * stored value instead (the project then holds nothing for it), so a reset knob leaves no trace.
 */
export function setParamCommand(
  device: Pick<Device, "id">,
  param: ParamDescriptorDto,
  value: number,
): EditCommand {
  const v = clampParam(param, value);
  return {
    command: "set_device_param",
    device: device.id,
    param: param.id,
    value: v === param.default ? null : v,
  };
}

/** The descriptor of the device's kind, if this build has one. */
export function descriptorFor(
  descriptors: readonly DeviceDescriptor[],
  device: Pick<Device, "kind">,
): DeviceDescriptor | undefined {
  return descriptors.find((d) => d.kind === device.kind);
}

/** One row of a generated panel. */
export interface PanelControl {
  param: ParamDescriptorDto;
  /** The current value. */
  value: number;
  /** Where it sits on the control's travel, 0 to 1. */
  position: number;
  /** The value as text. */
  text: string;
  /** A selector (options) rather than a knob. */
  choice: boolean;
  /** A choice with few options, shown as a row of buttons instead of a drop-down. */
  segmented: boolean;
  /** The name to show: the parameter name without its section's name as a prefix. */
  label: string;
}

/** The most options a choice can have and still be shown as a row of buttons. */
export const SEGMENTED_MAX = 4;

/**
 * The name shown next to a control: `Osc 2 Level` in the section `Osc 2` reads `Level`. A name that
 * does not start with its section's name is shown whole.
 */
export function shortName(param: Pick<ParamDescriptorDto, "name" | "group">): string {
  const group = param.group;
  if (group && param.name.startsWith(`${group} `) && param.name.length > group.length + 1) {
    return param.name.slice(group.length + 1);
  }
  return param.name;
}

/** One section of a generated panel: the controls that share a `group`. */
export interface PanelGroup {
  /** The section name; empty for parameters without one. */
  name: string;
  controls: PanelControl[];
}

/** The controls of a device's panel, in the descriptor's order. */
export function panelControls(
  device: Pick<Device, "params">,
  descriptor: Pick<DeviceDescriptor, "params">,
): PanelControl[] {
  return descriptor.params.map((param) => {
    const value = paramValue(device, param);
    return {
      param,
      value,
      position: valueToPosition(param, value),
      text: formatParam(param, value),
      choice: isChoice(param),
      segmented: isChoice(param) && param.choices.length <= SEGMENTED_MAX,
      label: shortName(param),
    };
  });
}

/**
 * The controls of a device's panel split into sections, in the order each section's first
 * parameter appears; parameters keep their descriptor order within a section. A device whose
 * parameters have no group gets one section with an empty name.
 */
export function panelGroups(
  device: Pick<Device, "params">,
  descriptor: Pick<DeviceDescriptor, "params">,
): PanelGroup[] {
  const groups: PanelGroup[] = [];
  for (const control of panelControls(device, descriptor)) {
    const name = control.param.group ?? "";
    let group = groups.find((g) => g.name === name);
    if (!group) {
      group = { name, controls: [] };
      groups.push(group);
    }
    group.controls.push(control);
  }
  return groups;
}

/** A new device of `descriptor` with every parameter at its default. */
export function newDevice(descriptor: DeviceDescriptor, id: number): Device {
  return {
    id,
    kind: descriptor.kind,
    name: descriptor.name,
    enabled: true,
    params: {},
    state: [],
    sidechain: null,
  };
}
