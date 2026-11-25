//! The `project.json` format: versioned JSON, forward migration, validation on load.
//!
//! A project file is untrusted input. [`from_json`] bounds its size, parses it, migrates old
//! versions step by step, deserializes through the validating constructors and then runs
//! [`Project::validate`]; it returns an error for anything malformed and never panics.

use std::fmt;

use serde::Serialize;
use serde_json::Value;

use crate::migrate;
use crate::model::Project;
use crate::validate::ValidationError;

/// The format version this build writes.
///
/// Policy: any change to the model's serialized shape, including a purely additive field, bumps
/// this number, adds the `vN -> vN+1` function to [`migrate::CHAIN`] and commits a golden file
/// `tests/fixtures/vN+1/project.json`. A build refuses a file with a higher version
/// ([`FormatError::TooNew`]) rather than reading it and silently dropping fields it does not
/// know on the next save.
pub const FORMAT_VERSION: u32 = 2;

/// Largest `project.json` accepted, in bytes.
pub const MAX_PROJECT_BYTES: usize = 64 * 1024 * 1024;

/// Why a project file was rejected.
#[derive(Debug)]
pub enum FormatError {
    /// Larger than [`MAX_PROJECT_BYTES`].
    TooLarge,
    /// Not valid JSON, or not the expected shape.
    Json(serde_json::Error),
    /// No `format_version`, or not a positive integer.
    MissingVersion,
    /// Written by a newer version of the app.
    TooNew {
        /// Version in the file.
        found: u64,
        /// Newest version this build reads.
        supported: u32,
    },
    /// A migration step failed or is missing.
    Migration(String),
    /// Parsed but breaks a model invariant.
    Invalid(ValidationError),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "project file is larger than {MAX_PROJECT_BYTES} bytes"),
            Self::Json(e) => write!(f, "project file is not valid: {e}"),
            Self::MissingVersion => f.write_str("project file has no valid format_version"),
            Self::TooNew { found, supported } => write!(
                f,
                "project format version {found} is newer than this app supports ({supported}); update the app"
            ),
            Self::Migration(why) => write!(f, "could not migrate the project: {why}"),
            Self::Invalid(e) => write!(f, "project file breaks an invariant: {e}"),
        }
    }
}

impl std::error::Error for FormatError {}

impl From<ValidationError> for FormatError {
    fn from(error: ValidationError) -> Self {
        Self::Invalid(error)
    }
}

#[derive(Serialize)]
struct FileRef<'a> {
    format_version: u32,
    #[serde(flatten)]
    project: &'a Project,
}

/// Serializes `project` as pretty-printed `project.json` content (after validating it, so a
/// file this writes always loads).
pub fn to_json(project: &Project) -> Result<Vec<u8>, FormatError> {
    project.validate()?;
    let file = FileRef {
        format_version: FORMAT_VERSION,
        project,
    };
    let mut bytes = serde_json::to_vec_pretty(&file).map_err(FormatError::Json)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[derive(serde::Deserialize)]
struct Peek {
    format_version: Option<u64>,
}

/// The `format_version` of `project.json` content, if it has a well-formed one.
pub fn peek_version(bytes: &[u8]) -> Option<u64> {
    serde_json::from_slice::<Peek>(bytes).ok()?.format_version
}

/// Parses, migrates and validates `project.json` content.
///
/// A file at the current version is deserialized directly; older ones go through a generic
/// JSON tree so the migration steps can rewrite them.
pub fn from_json(bytes: &[u8]) -> Result<Project, FormatError> {
    if bytes.len() > MAX_PROJECT_BYTES {
        return Err(FormatError::TooLarge);
    }
    if peek_version(bytes) == Some(u64::from(FORMAT_VERSION)) {
        let project: Project = serde_json::from_slice(bytes).map_err(FormatError::Json)?;
        project.validate()?;
        return Ok(project);
    }
    let mut value: Value = serde_json::from_slice(bytes).map_err(FormatError::Json)?;
    let version = value
        .get("format_version")
        .and_then(Value::as_u64)
        .filter(|v| *v >= 1)
        .ok_or(FormatError::MissingVersion)?;
    if version > u64::from(FORMAT_VERSION) {
        return Err(FormatError::TooNew {
            found: version,
            supported: FORMAT_VERSION,
        });
    }
    // `version` is in 1..=FORMAT_VERSION here.
    migrate::migrate(&mut value, version as u32)?;
    let project: Project = serde_json::from_value(value).map_err(FormatError::Json)?;
    project.validate()?;
    Ok(project)
}
