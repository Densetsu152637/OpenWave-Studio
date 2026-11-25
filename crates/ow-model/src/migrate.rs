//! Forward migration of `project.json` between format versions.
//!
//! Each step rewrites the parsed JSON of version `N` into version `N + 1` before it is
//! deserialized into the current model, so the model never carries old shapes. A project file
//! from version `N` therefore keeps loading as long as the chain from `N` to
//! [`FORMAT_VERSION`] is complete. Every version keeps a golden file under
//! `tests/fixtures/` that the `serde` tests load.

use serde_json::Value;

use crate::format::{FORMAT_VERSION, FormatError};

/// One migration step: rewrites the document in place from version `N` to `N + 1` (the runner
/// updates `format_version`).
pub type Migration = fn(&mut Value) -> Result<(), String>;

/// The oldest format version ever written; `CHAIN[0]` migrates it to the next.
pub const FIRST_VERSION: u32 = 1;

/// The steps from [`FIRST_VERSION`] up to [`FORMAT_VERSION`]: entry `i` migrates version
/// `FIRST_VERSION + i`.
pub const CHAIN: &[Migration] = &[v1_to_v2];

/// Version 2 adds the recording fields: a track's `input` and `monitor`, a clip's `take` and
/// the settings' `punch_range` and `punch_enabled`. All have serde defaults that mean what a
/// version 1 file implies (default input, no monitoring, no take, no punch), so nothing is
/// rewritten; the step exists so the chain is complete and the version moves on.
fn v1_to_v2(doc: &mut Value) -> Result<(), String> {
    if doc.is_object() {
        Ok(())
    } else {
        Err("not an object".into())
    }
}

/// Migrates `value` from `from` to the current version using the built-in chain.
pub fn migrate(value: &mut Value, from: u32) -> Result<(), FormatError> {
    migrate_chain(value, from, FORMAT_VERSION, FIRST_VERSION, CHAIN)
}

/// Runs the steps of `chain` (whose first entry migrates `chain_start`) from version `from`
/// up to `target`, updating `format_version` after each step.
pub fn migrate_chain(
    value: &mut Value,
    from: u32,
    target: u32,
    chain_start: u32,
    chain: &[Migration],
) -> Result<(), FormatError> {
    for version in from..target {
        let step = version
            .checked_sub(chain_start)
            .and_then(|index| chain.get(index as usize))
            .ok_or_else(|| {
                FormatError::Migration(format!("no migration from version {version}"))
            })?;
        step(value).map_err(|why| {
            FormatError::Migration(format!("version {version} to {}: {why}", version + 1))
        })?;
        match value.as_object_mut() {
            Some(object) => {
                object.insert("format_version".into(), Value::from(version + 1));
            }
            None => {
                return Err(FormatError::Migration(
                    "the project is not a JSON object".into(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_chain_reaches_the_current_version() {
        assert_eq!(CHAIN.len() as u32, FORMAT_VERSION - FIRST_VERSION);
    }
}
