//! Invariants of a [`Project`], checked on load, on save and by every edit command.
//!
//! A project file is untrusted input: validation must never panic and must reject anything that
//! could later index out of range, loop forever or escape the bundle directory.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::ids::DeviceId;
use crate::model::{
    AutomationLane, AutomationTarget, AuxSend, Bus, Clip, ClipContent, Device, InputChannels,
    Marker, MediaItem, MixerRef, Note, Output, Pattern, Project, ProjectSettings, Strip, Track,
    TrackKind,
};

/// Longest name, in characters.
pub const MAX_NAME_CHARS: usize = 256;
/// Largest position or length, in beats.
pub const MAX_BEAT: f64 = 1.0e9;
/// Lowest gain accepted, in dB.
pub const MIN_GAIN_DB: f64 = -144.0;
/// Highest gain accepted, in dB.
pub const MAX_GAIN_DB: f64 = 48.0;
/// Longest device kind, format tag or media path, in bytes.
const MAX_TAG_BYTES: usize = 512;

/// Input channels a track can name (the first channel of its input), exclusive.
pub const MAX_INPUT_CHANNELS: u16 = 256;
/// Largest id an entity may have: 2^53 - 1, so ids survive a round trip through JavaScript
/// numbers in the UI.
pub const MAX_ID: u64 = (1 << 53) - 1;
/// Highest sample rate accepted.
const MAX_SAMPLE_RATE: u32 = 768_000;
/// Lowest sample rate accepted.
const MIN_SAMPLE_RATE: u32 = 8_000;

/// Why a project, entity or value was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Where, such as `clips[12].start`.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for ValidationError {}

type Result<T = ()> = std::result::Result<T, ValidationError>;

fn bad<T>(path: impl Into<String>, message: impl Into<String>) -> Result<T> {
    Err(ValidationError {
        path: path.into(),
        message: message.into(),
    })
}

pub(crate) fn check_name(path: &str, value: &str) -> Result {
    if value.chars().take(MAX_NAME_CHARS + 1).count() > MAX_NAME_CHARS {
        return bad(path, format!("longer than {MAX_NAME_CHARS} characters"));
    }
    if value.chars().any(char::is_control) {
        return bad(path, "contains a control character");
    }
    Ok(())
}

pub(crate) fn check_color(path: &str, value: u32) -> Result {
    if value > 0x00FF_FFFF {
        return bad(path, "not a 0xRRGGBB color");
    }
    Ok(())
}

fn ranged(path: &str, value: f64, min: f64, max: f64) -> Result {
    if !value.is_finite() || value < min || value > max {
        return bad(path, format!("must be a number from {min} to {max}"));
    }
    Ok(())
}

fn position(path: &str, value: f64) -> Result {
    ranged(path, value, 0.0, MAX_BEAT)
}

fn length(path: &str, value: f64) -> Result {
    if !value.is_finite() || value <= 0.0 || value > MAX_BEAT {
        return bad(
            path,
            format!("must be greater than 0 and at most {MAX_BEAT}"),
        );
    }
    Ok(())
}

/// True for a relative media path under `audio/` that cannot leave the bundle on any platform.
pub fn is_safe_media_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("audio/") else {
        return false;
    };
    if path.len() > MAX_TAG_BYTES {
        return false;
    }
    rest.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && !is_reserved_windows_name(segment)
            && !segment.ends_with(['.', ' '])
            && !segment
                .chars()
                .any(|c| c.is_control() || "\\:*?\"<>|".contains(c))
    })
}

/// Windows device names (`CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`, also with the
/// superscript digits 1 to 3) cannot be file names whatever the extension: the stem before the
/// first dot, trimmed of trailing spaces and compared case-insensitively, decides.
fn is_reserved_windows_name(segment: &str) -> bool {
    let stem = segment
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ');
    let upper = stem.to_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let mut chars = upper.chars();
    let head: String = chars.by_ref().take(3).collect();
    if head != "COM" && head != "LPT" {
        return false;
    }
    match (chars.next(), chars.next()) {
        (Some(d), None) => d.is_ascii_digit() || matches!(d, '\u{b9}' | '\u{b2}' | '\u{b3}'),
        _ => false,
    }
}

fn check_note(path: &str, note: &Note) -> Result {
    position(&format!("{path}.start"), note.start)?;
    length(&format!("{path}.length"), note.length)?;
    if note.key > 127 {
        return bad(format!("{path}.key"), "must be 0 to 127");
    }
    if !(1..=127).contains(&note.velocity) {
        return bad(format!("{path}.velocity"), "must be 1 to 127");
    }
    Ok(())
}

pub(crate) fn check_settings(settings: &ProjectSettings) -> Result {
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&settings.sample_rate) {
        return bad(
            "settings.sample_rate",
            format!("must be {MIN_SAMPLE_RATE} to {MAX_SAMPLE_RATE}"),
        );
    }
    if settings.count_in_bars > 16 {
        return bad("settings.count_in_bars", "must be 0 to 16");
    }
    if let Some(range) = settings.loop_range {
        position("settings.loop_range.start", range.start)?;
        position("settings.loop_range.end", range.end)?;
        if range.end <= range.start {
            return bad("settings.loop_range", "must end after it starts");
        }
    }
    if let Some(range) = settings.punch_range {
        position("settings.punch_range.start", range.start)?;
        position("settings.punch_range.end", range.end)?;
        if range.end <= range.start {
            return bad("settings.punch_range", "must end after it starts");
        }
    }
    Ok(())
}

pub(crate) fn check_marker(marker: &Marker) -> Result {
    check_name("marker.name", &marker.name)?;
    position("marker.beat", marker.beat)?;
    check_color("marker.color", marker.color)
}

pub(crate) fn check_pattern(pattern: &Pattern) -> Result {
    check_name("pattern.name", &pattern.name)?;
    check_color("pattern.color", pattern.color)?;
    length("pattern.length", pattern.length)?;
    for (index, note) in pattern.notes.iter().enumerate() {
        check_note(&format!("pattern.notes[{index}]"), note)?;
    }
    Ok(())
}

pub(crate) fn check_media(item: &MediaItem) -> Result {
    check_name("media.name", &item.name)?;
    if !is_safe_media_path(&item.path) {
        return bad("media.path", "must be a plain relative path under audio/");
    }
    if item.hash.is_empty()
        || item.hash.len() > 128
        || !item.hash.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return bad("media.hash", "must be 1 to 128 alphanumeric characters");
    }
    if !(1..=MAX_SAMPLE_RATE).contains(&item.sample_rate) {
        return bad("media.sample_rate", "out of range");
    }
    if !(1..=256).contains(&item.channels) {
        return bad("media.channels", "must be 1 to 256");
    }
    Ok(())
}

impl Project {
    /// Checks every invariant: finite, in-range values; unique ids below `next_id`; references
    /// that resolve; clip content matching its track; every device in exactly one slot; no
    /// routing cycles.
    pub fn validate(&self) -> Result {
        check_name("name", &self.name)?;
        check_settings(&self.settings)?;
        self.check_ids()?;
        for (index, track) in self.tracks.iter().enumerate() {
            self.check_track(track)
                .map_err(|e| prefix(&format!("tracks[{index}]"), e))?;
        }
        for (index, bus) in self.buses.iter().enumerate() {
            self.check_bus(bus)
                .map_err(|e| prefix(&format!("buses[{index}]"), e))?;
        }
        self.check_strip("master", &self.master, true)?;
        for clip in self.clips.values() {
            self.check_clip(clip)
                .map_err(|e| prefix(&format!("clips[{}]", clip.id.0), e))?;
        }
        for pattern in self.patterns.values() {
            check_pattern(pattern)
                .map_err(|e| prefix(&format!("patterns[{}]", pattern.id.0), e))?;
        }
        for item in self.media_pool.values() {
            check_media(item).map_err(|e| prefix(&format!("media_pool[{}]", item.id.0), e))?;
        }
        for device in self.devices.values() {
            self.check_device(device)
                .map_err(|e| prefix(&format!("devices[{}]", device.id.0), e))?;
        }
        for lane in self.automation.values() {
            self.check_lane(lane)
                .map_err(|e| prefix(&format!("automation[{}]", lane.id.0), e))?;
        }
        for marker in self.markers.values() {
            check_marker(marker).map_err(|e| prefix(&format!("markers[{}]", marker.id.0), e))?;
        }
        self.check_device_ownership()?;
        if !self.routing_is_acyclic() {
            return bad("routing", "buses route into each other in a cycle");
        }
        Ok(())
    }

    fn check_ids(&self) -> Result {
        let mut seen = HashSet::new();
        let ids = self
            .tracks
            .iter()
            .map(|t| t.id.0)
            .chain(self.buses.iter().map(|b| b.id.0))
            .chain(self.clips.keys().map(|k| k.0))
            .chain(self.patterns.keys().map(|k| k.0))
            .chain(self.media_pool.keys().map(|k| k.0))
            .chain(self.devices.keys().map(|k| k.0))
            .chain(self.automation.keys().map(|k| k.0))
            .chain(self.markers.keys().map(|k| k.0));
        for id in ids {
            if !seen.insert(id) {
                return bad("ids", format!("id {id} is used by more than one entity"));
            }
            if id > MAX_ID {
                return bad("ids", format!("id {id} is above {MAX_ID}"));
            }
            if id >= self.next_id {
                return bad("ids", format!("id {id} is not below next_id"));
            }
        }
        if self.next_id > MAX_ID + 1 {
            return bad("next_id", format!("must be at most {}", MAX_ID + 1));
        }
        let mut paths = HashSet::new();
        for item in self.media_pool.values() {
            if !paths.insert(item.path.to_lowercase()) {
                return bad(
                    "media_pool",
                    format!("path {:?} is used twice (case-insensitively)", item.path),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn check_strip(&self, path: &str, strip: &Strip, master: bool) -> Result {
        let s = &strip.settings;
        ranged(
            &format!("{path}.gain_db"),
            s.gain_db,
            MIN_GAIN_DB,
            MAX_GAIN_DB,
        )?;
        ranged(&format!("{path}.pan"), s.pan, -1.0, 1.0)?;
        match s.output {
            Output::Master => {}
            Output::Bus(bus) => {
                if master {
                    return bad(format!("{path}.output"), "the master has no other output");
                }
                if self.bus(bus).is_none() {
                    return bad(format!("{path}.output"), format!("unknown bus {}", bus.0));
                }
            }
        }
        if master && !strip.sends.is_empty() {
            return bad(format!("{path}.sends"), "the master has no sends");
        }
        for (index, send) in strip.sends.iter().enumerate() {
            self.check_send(&format!("{path}.sends[{index}]"), send)?;
        }
        for (index, device) in strip.inserts.iter().enumerate() {
            if !self.devices.contains_key(device) {
                return bad(
                    format!("{path}.inserts[{index}]"),
                    format!("unknown device {}", device.0),
                );
            }
        }
        Ok(())
    }

    fn check_send(&self, path: &str, send: &AuxSend) -> Result {
        if self.bus(send.target).is_none() {
            return bad(path, format!("unknown bus {}", send.target.0));
        }
        ranged(
            &format!("{path}.level_db"),
            send.level_db,
            MIN_GAIN_DB,
            MAX_GAIN_DB,
        )
    }

    pub(crate) fn check_track(&self, track: &Track) -> Result {
        check_name("name", &track.name)?;
        check_color("color", track.color)?;
        self.check_strip("strip", &track.strip, false)?;
        if let Some(instrument) = track.instrument {
            if track.kind != TrackKind::Midi {
                return bad("instrument", "only MIDI tracks have an instrument");
            }
            if !self.devices.contains_key(&instrument) {
                return bad("instrument", format!("unknown device {}", instrument.0));
            }
        }
        if let Some(input) = track.input {
            if track.kind != TrackKind::Audio {
                return bad("input", "only audio tracks have an input");
            }
            let (InputChannels::Mono(first) | InputChannels::Stereo(first)) = input.channels;
            if first >= MAX_INPUT_CHANNELS {
                return bad(
                    "input",
                    format!("channel must be below {MAX_INPUT_CHANNELS}"),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn check_bus(&self, bus: &Bus) -> Result {
        check_name("name", &bus.name)?;
        check_color("color", bus.color)?;
        self.check_strip("strip", &bus.strip, false)
    }

    pub(crate) fn check_clip(&self, clip: &Clip) -> Result {
        check_name("name", &clip.name)?;
        position("start", clip.start)?;
        length("length", clip.length)?;
        let Some(track) = self.track(clip.track) else {
            return bad("track", format!("unknown track {}", clip.track.0));
        };
        if let Some(take) = clip.take {
            if !matches!(clip.content, ClipContent::Audio { .. }) {
                return bad("take", "only audio clips can be takes");
            }
            if take.active
                && self.clips.values().any(|other| {
                    other.id != clip.id
                        && other.track == clip.track
                        && other
                            .take
                            .is_some_and(|t| t.group == take.group && t.active)
                })
            {
                return bad("take", "another take of this group is already active");
            }
        }
        match &clip.content {
            ClipContent::Audio {
                media,
                gain_db,
                fade_in,
                fade_out,
                ..
            } => {
                if track.kind != TrackKind::Audio {
                    return bad("content", "audio clips belong on audio tracks");
                }
                if !self.media_pool.contains_key(media) {
                    return bad("content.media", format!("unknown media {}", media.0));
                }
                ranged("content.gain_db", *gain_db, MIN_GAIN_DB, MAX_GAIN_DB)?;
                position("content.fade_in", *fade_in)?;
                position("content.fade_out", *fade_out)?;
            }
            ClipContent::Midi { notes, controllers } => {
                if track.kind != TrackKind::Midi {
                    return bad("content", "MIDI clips belong on MIDI tracks");
                }
                for (index, note) in notes.iter().enumerate() {
                    check_note(&format!("content.notes[{index}]"), note)?;
                }
                for (index, cc) in controllers.iter().enumerate() {
                    position(&format!("content.controllers[{index}].beat"), cc.beat)?;
                    if cc.controller > 127 || cc.value > 127 {
                        return bad(format!("content.controllers[{index}]"), "must be 0 to 127");
                    }
                }
            }
            ClipContent::Pattern { pattern } => {
                if track.kind != TrackKind::Midi {
                    return bad("content", "pattern instances belong on MIDI tracks");
                }
                if !self.patterns.contains_key(pattern) {
                    return bad("content.pattern", format!("unknown pattern {}", pattern.0));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn check_device(&self, device: &Device) -> Result {
        check_name("name", &device.name)?;
        if device.kind.is_empty() || device.kind.len() > MAX_TAG_BYTES {
            return bad("kind", "must be 1 to 512 bytes");
        }
        for (param, value) in &device.params {
            if !value.is_finite() {
                return bad(format!("params[{param}]"), "must be finite");
            }
        }
        for (index, chunk) in device.state.iter().enumerate() {
            if chunk.format.is_empty() || chunk.format.len() > 64 {
                return bad(format!("state[{index}].format"), "must be 1 to 64 bytes");
            }
        }
        if let Some(source) = device.sidechain
            && self.strip(source).is_none()
        {
            return bad("sidechain", "unknown source strip");
        }
        Ok(())
    }

    pub(crate) fn check_lane(&self, lane: &AutomationLane) -> Result {
        match lane.target {
            AutomationTarget::Device { device, .. } => {
                if !self.devices.contains_key(&device) {
                    return bad("target", format!("unknown device {}", device.0));
                }
            }
            AutomationTarget::Strip { strip, .. } => {
                if self.strip(strip).is_none() {
                    return bad("target", "unknown strip");
                }
            }
        }
        let mut previous = 0.0_f64;
        for (index, point) in lane.points.iter().enumerate() {
            let path = format!("points[{index}]");
            position(&format!("{path}.beat"), point.beat)?;
            if point.beat < previous {
                return bad(format!("{path}.beat"), "points must be ordered by position");
            }
            previous = point.beat;
            ranged(&format!("{path}.value"), point.value, -1.0e12, 1.0e12)?;
            ranged(&format!("{path}.curve"), point.curve, -1.0, 1.0)?;
        }
        Ok(())
    }

    /// Every device sits in exactly one insert chain or instrument slot.
    fn check_device_ownership(&self) -> Result {
        let mut owners: HashMap<u64, u32> = HashMap::new();
        let mut count = |id: u64| *owners.entry(id).or_insert(0) += 1;
        for track in &self.tracks {
            track.strip.inserts.iter().for_each(|d| count(d.0));
            track.instrument.iter().for_each(|d| count(d.0));
        }
        for bus in &self.buses {
            bus.strip.inserts.iter().for_each(|d| count(d.0));
        }
        self.master.inserts.iter().for_each(|d| count(d.0));
        for id in self.devices.keys() {
            match owners.get(&id.0).copied().unwrap_or(0) {
                1 => {}
                0 => return bad("devices", format!("device {} is in no slot", id.0)),
                _ => {
                    return bad(
                        "devices",
                        format!("device {} is in more than one slot", id.0),
                    );
                }
            }
        }
        Ok(())
    }

    /// True when signal flow never returns to a strip: following outputs, sends and sidechain
    /// sources (a source feeds the strip holding the device) finds no cycle. Iterative, so a long
    /// chain in a hostile file cannot overflow the stack.
    pub(crate) fn routing_is_acyclic(&self) -> bool {
        let mut edges: HashMap<MixerRef, Vec<MixerRef>> = HashMap::new();
        let mut nodes: Vec<MixerRef> = vec![MixerRef::Master];
        let mut add_strip = |at: MixerRef, strip: &Strip, instrument: Option<DeviceId>| {
            nodes.push(at);
            let targets = edges.entry(at).or_default();
            if let Output::Bus(target) = strip.settings.output {
                targets.push(MixerRef::Bus(target));
            }
            targets.extend(strip.sends.iter().map(|s| MixerRef::Bus(s.target)));
            let devices = strip.inserts.iter().copied().chain(instrument);
            let mut sources = Vec::new();
            for device in devices {
                if let Some(source) = self.devices.get(&device).and_then(|d| d.sidechain) {
                    sources.push(source);
                }
            }
            sources
        };
        let mut sidechains: Vec<(MixerRef, MixerRef)> = Vec::new();
        for track in &self.tracks {
            let at = MixerRef::Track(track.id);
            for source in add_strip(at, &track.strip, track.instrument) {
                sidechains.push((source, at));
            }
        }
        for bus in &self.buses {
            let at = MixerRef::Bus(bus.id);
            for source in add_strip(at, &bus.strip, None) {
                sidechains.push((source, at));
            }
        }
        for source in add_strip(MixerRef::Master, &self.master, None) {
            sidechains.push((source, MixerRef::Master));
        }
        for (source, owner) in sidechains {
            edges.entry(source).or_default().push(owner);
        }
        // 0 = unvisited, 1 = on the current path, 2 = done.
        let mut state: HashMap<MixerRef, u8> = HashMap::new();
        for start in nodes {
            if state.get(&start).copied().unwrap_or(0) != 0 {
                continue;
            }
            let mut stack = vec![(start, 0_usize)];
            state.insert(start, 1);
            while let Some((node, next)) = stack.last().copied() {
                let targets = edges.get(&node).map(Vec::as_slice).unwrap_or(&[]);
                if let Some(&target) = targets.get(next) {
                    if let Some(top) = stack.last_mut() {
                        top.1 += 1;
                    }
                    match state.get(&target).copied().unwrap_or(0) {
                        1 => return false,
                        0 => {
                            state.insert(target, 1);
                            stack.push((target, 0));
                        }
                        _ => {}
                    }
                } else {
                    state.insert(node, 2);
                    stack.pop();
                }
            }
        }
        true
    }
}

fn prefix(base: &str, error: ValidationError) -> ValidationError {
    ValidationError {
        path: format!("{base}.{}", error.path),
        message: error.message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_paths_cannot_escape_the_bundle() {
        assert!(is_safe_media_path("audio/take 1.wav"));
        assert!(is_safe_media_path("audio/ab/cd.flac"));
        for bad in [
            "",
            "audio/",
            "/etc/passwd",
            "../audio/x.wav",
            "audio/../x.wav",
            "audio/./x.wav",
            "audio//x.wav",
            "audio\\x.wav",
            "audio/C:evil",
            "other/x.wav",
            "audio/x.wav.",
            "audio/CON",
            "audio/con.wav",
            "audio/Nul.tar.gz",
            "audio/com1.wav",
            "audio/LPT9 .wav",
            "audio/COM\u{b9}.wav",
            "audio/sub/aux.txt",
        ] {
            assert!(!is_safe_media_path(bad), "{bad:?}");
        }
    }
}
