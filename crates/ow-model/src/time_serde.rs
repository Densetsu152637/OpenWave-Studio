//! Serialization of the timeline types.
//!
//! The time types keep their fields private so a value is always valid; deserialization
//! therefore goes through their validating constructors and rejects what they reject.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::time::{TempoMap, TempoPoint, TimeSignature, TimeSignatureMap};

#[derive(Serialize, Deserialize)]
struct PointRepr {
    beat: f64,
    bpm: f64,
    #[serde(default)]
    ramp: bool,
}

impl Serialize for TempoMap {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.points().iter().map(|point| PointRepr {
            beat: point.beat,
            bpm: point.bpm,
            ramp: point.ramp,
        }))
    }
}

impl<'de> Deserialize<'de> for TempoMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let points = Vec::<PointRepr>::deserialize(deserializer)?;
        let points = points
            .into_iter()
            .map(|p| TempoPoint {
                beat: p.beat,
                bpm: p.bpm,
                ramp: p.ramp,
            })
            .collect();
        TempoMap::new(points).map_err(D::Error::custom)
    }
}

#[derive(Serialize, Deserialize)]
struct SignatureRepr {
    numerator: u32,
    denominator: u32,
}

impl Serialize for TimeSignature {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SignatureRepr {
            numerator: self.numerator(),
            denominator: self.denominator(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TimeSignature {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let repr = SignatureRepr::deserialize(deserializer)?;
        TimeSignature::new(repr.numerator, repr.denominator).map_err(D::Error::custom)
    }
}

#[derive(Serialize, Deserialize)]
struct ChangeRepr {
    bar: u32,
    numerator: u32,
    denominator: u32,
}

impl Serialize for TimeSignatureMap {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.changes().map(|(bar, signature)| ChangeRepr {
            bar,
            numerator: signature.numerator(),
            denominator: signature.denominator(),
        }))
    }
}

impl<'de> Deserialize<'de> for TimeSignatureMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let changes = Vec::<ChangeRepr>::deserialize(deserializer)?;
        let mut parsed = Vec::with_capacity(changes.len());
        for change in changes {
            let signature = TimeSignature::new(change.numerator, change.denominator)
                .map_err(D::Error::custom)?;
            parsed.push((change.bar, signature));
        }
        TimeSignatureMap::new(parsed).map_err(D::Error::custom)
    }
}
