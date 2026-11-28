//! Descriptions of the built-in devices and of scanned plugins, for the UI's generic device
//! panel.
//!
//! The data for built-in devices comes from the `ow-dsp` registry and for plugins from the
//! scanner (the app crate converts); this module only gives it a wire shape.
//! [`device_descriptors`] answers the `device_descriptors` query: every built-in kind with its
//! parameters (id, name, range, default, unit, scale, choice labels). The UI builds each
//! device's knobs and selectors from it, so a new built-in device needs no UI code. Parameter
//! values themselves live in the project (`Device::params`, edited with `SetDeviceParam`), never
//! in this list.

use ow_dsp::{DeviceInfo, ParamDescriptor, ParamKind, Scale, Unit};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What a parameter's number means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ParamUnit {
    /// A plain number.
    None,
    /// Decibels.
    Decibels,
    /// Hertz.
    Hertz,
    /// Seconds.
    Seconds,
    /// A percentage, 0 to 100.
    Percent,
    /// Semitones.
    Semitones,
    /// Cents (hundredths of a semitone).
    Cents,
}

/// How a control maps its travel to the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ParamScale {
    /// Evenly between `min` and `max`.
    Linear,
    /// Evenly in the logarithm (`min` is positive).
    Log,
}

/// One parameter of a device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ParamDescriptorDto {
    /// Stable id: the key of `Device::params`.
    pub id: u32,
    /// Name shown next to the control.
    pub name: String,
    /// Smallest value.
    pub min: f64,
    /// Largest value.
    pub max: f64,
    /// Value used when the project does not set the parameter.
    pub default: f64,
    /// What the number means.
    pub unit: ParamUnit,
    /// How a control maps its travel to the value.
    pub scale: ParamScale,
    /// Option labels when the parameter is a choice (the value is the option's index, 0 up);
    /// empty for a continuous parameter.
    pub choices: Vec<String>,
    /// The panel section the parameter sits in (sections appear in order of their first
    /// parameter); absent for an ungrouped parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub group: Option<String>,
}

/// One device kind: a built-in device or a scanned third-party plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DeviceDescriptor {
    /// The `Device::kind` string, for example `builtin.synth` or `clap:com.vendor.plugin` or `vst3:<class id>`.
    pub kind: String,
    /// Name shown in menus and panels.
    pub name: String,
    /// True for an instrument (note events in), false for an effect.
    pub instrument: bool,
    /// The parameters, in display order.
    pub params: Vec<ParamDescriptorDto>,
    /// The plugin format (`clap` or `vst3`); absent for a built-in device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub format: Option<String>,
    /// The plugin's vendor; absent for a built-in device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub vendor: Option<String>,
    /// True when the plugin has an editor window that this platform can show (Windows and
    /// macOS); absent otherwise, and for every built-in device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub editor: Option<bool>,
    /// True when the device takes a sidechain (key) input; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sidechain: Option<bool>,
}

/// A plugin file the last scan could not use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PluginScanFailure {
    /// The plugin file.
    pub path: String,
    /// `crashed`, `timed_out` or `failed`.
    pub status: String,
    /// What went wrong, as text.
    pub detail: String,
}

/// What the plugin scanner knows: every usable plugin and every file it had to give up on.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PluginScanSummary {
    /// The plugins that can be used.
    pub plugins: Vec<DeviceDescriptor>,
    /// Files that crashed the scanner, timed out or failed to load.
    pub failures: Vec<PluginScanFailure>,
    /// Plugin files the last scan found.
    pub files: u32,
    /// How many of them were scanned now (the rest came from the cache).
    pub scanned: u32,
    /// The folders that were searched.
    pub folders: Vec<String>,
}

impl From<&ParamDescriptor> for ParamDescriptorDto {
    fn from(param: &ParamDescriptor) -> Self {
        Self {
            id: param.id,
            name: param.name.to_string(),
            min: param.min,
            max: param.max,
            default: param.default,
            unit: match param.unit {
                Unit::None => ParamUnit::None,
                Unit::Decibels => ParamUnit::Decibels,
                Unit::Hertz => ParamUnit::Hertz,
                Unit::Seconds => ParamUnit::Seconds,
                Unit::Percent => ParamUnit::Percent,
                Unit::Semitones => ParamUnit::Semitones,
                Unit::Cents => ParamUnit::Cents,
            },
            scale: match param.scale {
                Scale::Linear => ParamScale::Linear,
                Scale::Log => ParamScale::Log,
            },
            choices: match param.kind {
                ParamKind::Continuous => Vec::new(),
                ParamKind::Choice(options) => options.iter().map(|o| (*o).to_string()).collect(),
            },
            group: (!param.group.is_empty()).then(|| param.group.to_string()),
        }
    }
}

impl From<&DeviceInfo> for DeviceDescriptor {
    fn from(info: &DeviceInfo) -> Self {
        Self {
            kind: info.kind.to_string(),
            name: info.name.to_string(),
            instrument: info.instrument,
            params: info.params.iter().map(ParamDescriptorDto::from).collect(),
            format: None,
            vendor: None,
            editor: None,
            sidechain: info.sidechain.then_some(true),
        }
    }
}

/// Every built-in device kind, in registry order.
pub fn device_descriptors() -> Vec<DeviceDescriptor> {
    ow_dsp::all().iter().map(DeviceDescriptor::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_synth_is_described_with_its_choices_and_scales() {
        let all = device_descriptors();
        let synth = all.iter().find(|d| d.kind == "builtin.synth").unwrap();
        assert!(synth.instrument);
        let waveform = synth.params.iter().find(|p| p.name == "Waveform").unwrap();
        assert_eq!(waveform.choices, ["Saw", "Square", "Sine", "Triangle"]);
        assert_eq!(waveform.max, 3.0);
        assert_eq!(waveform.group.as_deref(), Some("Osc 1"));
        let cutoff = synth.params.iter().find(|p| p.name == "Cutoff").unwrap();
        assert_eq!(
            (cutoff.scale, cutoff.unit),
            (ParamScale::Log, ParamUnit::Hertz)
        );
        assert!(cutoff.choices.is_empty());
    }
}
