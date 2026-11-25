//! The loop and sample library as the UI and the MCP tools see it (stage 22b): the entries of
//! the last scan, searches over them, scan progress and the audition.
//!
//! Plain data: the scan and the preview themselves live in `ow-media` and the app crate.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A musical key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LibraryKey {
    /// Pitch class of the tonic: 0 = C, 1 = C sharp, ... 11 = B.
    pub tonic: u8,
    /// Minor when true, major otherwise.
    pub minor: bool,
}

/// One file of the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LibraryEntryInfo {
    /// Absolute path of the file.
    pub path: String,
    /// File name.
    pub name: String,
    /// Folder the file is in.
    pub folder: String,
    /// Length in seconds (0 when the file could not be read).
    pub duration_seconds: f64,
    /// Sample rate of the file.
    pub sample_rate: u32,
    /// Channels of the file.
    pub channels: u16,
    /// The tempo to use: the one in the file name, else the detected one.
    pub bpm: Option<f64>,
    /// Whether `bpm` was detected from the audio (false: written in the file name).
    pub bpm_detected: bool,
    /// The key to use: the one in the file name, else the detected one.
    pub key: Option<LibraryKey>,
    /// Tags from folder and file names.
    pub tags: Vec<String>,
    /// Why the file could not be analysed, when so.
    pub error: Option<String>,
}

/// A library search; every given criterion must match.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(rename_all = "camelCase")]
pub struct LibraryQuery {
    /// Words that must each appear in the file name, folder or tags.
    pub text: String,
    /// Tags that must all be present.
    pub tags: Vec<String>,
    /// Lowest tempo.
    pub bpm_min: Option<f64>,
    /// Highest tempo.
    pub bpm_max: Option<f64>,
    /// Also accept half and double of the tempi in range.
    pub include_half_double: bool,
    /// Required key.
    pub key: Option<LibraryKey>,
    /// Most entries to answer (0 means 200; at most 2000).
    pub limit: u32,
}

/// What a search answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LibrarySearchReply {
    /// Files in the library.
    pub total: u32,
    /// Files that matched (the list below may be shorter, see `LibraryQuery::limit`).
    pub matched: u32,
    /// The matching files, in path order.
    pub entries: Vec<LibraryEntryInfo>,
    /// A scan is running; the list is the one from before it.
    pub scanning: bool,
}

/// What `library_scan` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum LibraryScanAction {
    /// Scan the library folders and wait for the result.
    Scan,
    /// Report the progress of the running scan, or the result of the last one.
    Status,
    /// Stop the running scan.
    Cancel,
}

/// Progress and result of a library scan.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LibraryScanStatus {
    /// A scan is running.
    pub scanning: bool,
    /// Files handled so far by the running scan.
    pub done: u32,
    /// Files the running scan found (0 until it has listed the folders).
    pub total: u32,
    /// Files in the library index.
    pub entries: u32,
    /// Files the last finished scan analysed (the rest came from the cache).
    pub analysed: u32,
    /// Files the last finished scan answered from the cache.
    pub cached: u32,
    /// Files in the index that could not be analysed.
    pub failed: u32,
    /// The last scan was stopped before it finished.
    pub cancelled: bool,
    /// The folders that were scanned (the library folders of the settings).
    pub roots: Vec<String>,
}

/// What a preview started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LibraryPreviewReply {
    /// A preview is playing.
    pub playing: bool,
    /// The tempo of the file, when it has one.
    pub source_bpm: Option<f64>,
    /// The project tempo the file was stretched to (equal to `source_bpm` or absent when it was
    /// not stretched).
    pub target_bpm: Option<f64>,
    /// Length of the preview in seconds.
    pub seconds: f64,
}
