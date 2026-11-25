//! Tags from folder names and file-name tokens such as `120bpm` and `Am`.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Lowest tempo accepted from a file name.
pub const MIN_NAME_BPM: f64 = 40.0;
/// Highest tempo accepted from a file name.
pub const MAX_NAME_BPM: f64 = 300.0;

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// A musical key: tonic pitch class and mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibraryKey {
    /// 0 = C, 1 = C sharp, ... 11 = B.
    pub tonic: u8,
    /// Minor when true, major otherwise.
    pub minor: bool,
}

impl LibraryKey {
    /// Short name such as `Am` or `F#`.
    pub fn short_name(&self) -> String {
        format!(
            "{}{}",
            NOTE_NAMES[usize::from(self.tonic % 12)],
            if self.minor { "m" } else { "" }
        )
    }

    /// Parses a key name such as `Am`, `C#min`, `Ebmajor` or `F# minor`; `None` when the text is
    /// not a complete key name. A bare note name is major.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let mut chars = text.chars();
        let letter = chars.next()?.to_ascii_uppercase();
        let base = match letter {
            'C' => 0,
            'D' => 2,
            'E' => 4,
            'F' => 5,
            'G' => 7,
            'A' => 9,
            'B' => 11,
            _ => return None,
        };
        let rest = chars.as_str();
        let (shift, rest) = if let Some(r) = rest.strip_prefix('#') {
            (1, r)
        } else if let Some(r) = rest.strip_prefix('b') {
            (11, r)
        } else {
            (0, rest)
        };
        let minor = match rest.trim().to_ascii_lowercase().as_str() {
            "" | "maj" | "major" => false,
            "m" | "min" | "minor" => true,
            _ => return None,
        };
        Some(Self {
            tonic: (base + shift) % 12,
            minor,
        })
    }
}

/// What the file name itself says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NameTokens {
    /// A tempo from a token such as `120bpm`, `128.5BPM`, or the pair `120` `bpm`.
    pub bpm: Option<f64>,
    /// A key from a token such as `Am`, `C#min`, `Ebmaj`, or the pair `A` `minor`.
    pub key: Option<LibraryKey>,
    /// The remaining words, lower case, without pure numbers and one-letter words.
    pub words: Vec<String>,
}

fn split_words(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '#' || c == '.'))
        .map(|w| w.trim_matches('.'))
        .filter(|w| !w.is_empty())
        .collect()
}

fn parse_bpm_number(text: &str) -> Option<f64> {
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let value: f64 = text.parse().ok()?;
    (MIN_NAME_BPM..=MAX_NAME_BPM)
        .contains(&value)
        .then_some(value)
}

/// Strict token form of a key: a note letter, optional `#` or `b`, and an explicit mode
/// (`m`, `min`, `minor`, `maj`, `major`), so words such as `Bass` or `Drums` never match and a
/// bare letter is not a key.
fn key_token(token: &str) -> Option<LibraryKey> {
    let mut chars = token.chars();
    if !chars.next().is_some_and(|c| ('A'..='G').contains(&c)) {
        return None;
    }
    let rest = chars.as_str();
    let mode = rest.strip_prefix(['#', 'b']).unwrap_or(rest);
    matches!(
        mode,
        "m" | "min" | "minor" | "Min" | "Minor" | "maj" | "major" | "Maj" | "Major"
    )
    .then(|| LibraryKey::parse(token))
    .flatten()
}

/// A bare note name with an upper-case letter and an optional `#` or `b`.
fn is_bare_note(token: &str) -> bool {
    let mut chars = token.chars();
    chars.next().is_some_and(|c| ('A'..='G').contains(&c))
        && matches!(chars.as_str(), "" | "#" | "b")
}

fn mode_word(token: &str) -> Option<bool> {
    match token.to_ascii_lowercase().as_str() {
        "min" | "minor" => Some(true),
        "maj" | "major" => Some(false),
        _ => None,
    }
}

/// Reads tempo, key and plain words from a file stem (the name without its extension).
pub fn parse_name(stem: &str) -> NameTokens {
    let tokens = split_words(stem);
    let mut found = NameTokens::default();
    let mut used = vec![false; tokens.len()];
    for (i, token) in tokens.iter().enumerate() {
        let lower = token.to_ascii_lowercase();
        if found.bpm.is_none() {
            if let Some(number) = lower.strip_suffix("bpm").and_then(parse_bpm_number) {
                found.bpm = Some(number);
                used[i] = true;
                continue;
            }
            if let Some(number) = lower.strip_prefix("bpm").and_then(parse_bpm_number) {
                found.bpm = Some(number);
                used[i] = true;
                continue;
            }
            if lower == "bpm" {
                let before = i.checked_sub(1).and_then(|p| parse_bpm_number(tokens[p]));
                let after = tokens.get(i + 1).and_then(|t| parse_bpm_number(t));
                if let Some(number) = before.or(after) {
                    found.bpm = Some(number);
                    used[i] = true;
                    if before.is_some() {
                        used[i - 1] = true;
                    } else {
                        used[i + 1] = true;
                    }
                    continue;
                }
            }
        }
        if found.key.is_none() {
            if let Some(key) = key_token(token) {
                found.key = Some(key);
                used[i] = true;
                continue;
            }
            // A note name followed by a separate mode word: `A` `minor`.
            if is_bare_note(token)
                && let Some(minor) = tokens.get(i + 1).and_then(|t| mode_word(t))
                && let Some(key) = LibraryKey::parse(token)
            {
                found.key = Some(LibraryKey { minor, ..key });
                used[i] = true;
                used[i + 1] = true;
            }
        }
    }
    for (token, used) in tokens.iter().zip(&used) {
        let word = token.to_lowercase();
        let plain = word.chars().all(|c| c.is_alphanumeric());
        if !used && plain && word.chars().count() >= 2 && !word.chars().all(|c| c.is_ascii_digit())
        {
            found.words.push(word);
        }
    }
    found
}

/// Tags for the file at `path` inside the library root `root`: the names of the folders from
/// the root's own name down to the file's folder, and the words of the file name, all lower
/// case, sorted and without duplicates.
pub fn path_tags(root: &Path, path: &Path) -> Vec<String> {
    let mut tags = Vec::new();
    let mut add = |text: &str| {
        for word in split_words(text) {
            let word = word.to_lowercase();
            if word.chars().count() >= 2 && !word.chars().all(|c| c.is_ascii_digit()) {
                tags.push(word);
            }
        }
    };
    if let Some(name) = root.file_name().and_then(|n| n.to_str()) {
        add(name);
    }
    if let Some(folders) = path
        .strip_prefix(root)
        .ok()
        .and_then(|relative| relative.parent())
    {
        for part in folders.components() {
            if let Some(text) = part.as_os_str().to_str() {
                add(text);
            }
        }
    }
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        tags.extend(parse_name(stem).words);
    }
    tags.sort();
    tags.dedup();
    tags
}
