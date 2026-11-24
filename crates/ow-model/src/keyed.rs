//! Serde adapter: an ordered map of entities is written as a list and read back into the map,
//! rejecting duplicate ids.

use std::collections::BTreeMap;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::model::Keyed;

pub fn serialize<S, V>(map: &BTreeMap<V::Id, V>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    V: Keyed + Serialize,
{
    serializer.collect_seq(map.values())
}

pub fn deserialize<'de, D, V>(deserializer: D) -> Result<BTreeMap<V::Id, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Keyed + Deserialize<'de>,
{
    let items = Vec::<V>::deserialize(deserializer)?;
    let mut map = BTreeMap::new();
    for item in items {
        let id = item.id();
        if map.insert(id, item).is_some() {
            return Err(D::Error::custom(format!("duplicate id {id:?}")));
        }
    }
    Ok(map)
}
