//! Library tests: file-name tokens, scan and tags, cache reuse and invalidation, search and
//! in-tempo previews, on WAV files generated in temporary folders.

use std::f64::consts::TAU;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::Pcm;

const RATE: u32 = 44_100;

struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1_u64 << 52) as f64 - 1.0
    }
}

/// A four-on-the-floor-ish drum loop (kick on one and three, snare on two and four, eighth hats).
fn drum_loop(bpm: f64, seconds: f64) -> Vec<f32> {
    let beat = 60.0 / bpm;
    let mut buf = vec![0.0_f32; (seconds * f64::from(RATE)) as usize];
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
    let mut hit = |start: f64, len: f64, gain: f64, kind: u8| {
        let first = (start * f64::from(RATE)) as usize;
        let mut previous = 0.0;
        for n in 0..(len * f64::from(RATE)) as usize {
            let t = n as f64 / f64::from(RATE);
            let white = noise.next();
            let sample = match kind {
                0 => {
                    let phase = TAU * (50.0 * t + 90.0 * 0.03 * (1.0 - (-t / 0.03).exp()));
                    phase.sin() * (-t / 0.07).exp()
                }
                1 => (0.8 * white + 0.3 * (TAU * 190.0 * t).sin()) * (-t / 0.04).exp(),
                _ => {
                    let high = white - previous;
                    previous = white;
                    0.5 * high * (-t / 0.01).exp()
                }
            };
            if let Some(slot) = buf.get_mut(first + n) {
                *slot += (sample * gain) as f32;
            }
        }
    };
    let beats = (seconds / beat) as usize;
    for b in 0..beats {
        let at = 0.1 + b as f64 * beat;
        match b % 4 {
            0 => hit(at, 0.18, 0.9, 0),
            2 => hit(at, 0.18, 0.6, 0),
            _ => hit(at, 0.14, 0.5, 1),
        }
        hit(at, 0.04, 0.3, 2);
        hit(at + 0.5 * beat, 0.04, 0.2, 2);
    }
    buf
}

/// A minor progression Am Dm E Am with overtones.
fn a_minor_loop(seconds_per_chord: f64) -> Vec<f32> {
    let chords: [[f64; 3]; 4] = [
        [57.0, 60.0, 64.0],
        [50.0, 53.0, 57.0],
        [52.0, 56.0, 59.0],
        [57.0, 60.0, 64.0],
    ];
    let frames = (seconds_per_chord * f64::from(RATE)) as usize;
    let mut out = Vec::new();
    for chord in chords {
        let mut block = vec![0.0_f32; frames];
        for midi in chord {
            let freq = 440.0 * 2.0_f64.powf((midi - 69.0) / 12.0);
            for (i, s) in block.iter_mut().enumerate() {
                let t = i as f64 / f64::from(RATE);
                let tone: f64 = (1..=4)
                    .map(|h| (TAU * freq * f64::from(h) * t).sin() / f64::from(h))
                    .sum();
                *s += (0.15 * tone) as f32;
            }
        }
        out.extend(block);
    }
    out
}

fn write_wav(path: &Path, channels: &[Vec<f32>], rate: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let spec = hound::WavSpec {
        channels: channels.len() as u16,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..channels[0].len() {
        for channel in channels {
            writer.write_sample(channel[i]).unwrap();
        }
    }
    writer.finalize().unwrap();
}

/// A library folder with a drum loop, a tagged bass loop, a short one-shot, junk and a text file.
fn make_library(root: &Path) {
    write_wav(
        &root.join("House/loop_a.wav"),
        &[drum_loop(120.0, 12.0)],
        RATE,
    );
    let stereo = a_minor_loop(3.0);
    write_wav(
        &root.join("Bass/Am_dark_bass.wav"),
        &[stereo.clone(), stereo],
        RATE,
    );
    write_wav(
        &root.join("Drums/One Shots/Kick_01.wav"),
        &[drum_loop(60.0, 0.3)],
        48_000,
    );
    write_wav(
        &root.join("Drums/Loops/90bpm_boom_bap.wav"),
        &[drum_loop(90.0, 10.0)],
        RATE,
    );
    fs::create_dir_all(root.join("Misc")).unwrap();
    fs::write(root.join("Misc/broken.wav"), b"RIFF not really audio").unwrap();
    fs::write(root.join("Misc/readme.txt"), b"not audio").unwrap();
}

fn options(cache: Option<PathBuf>) -> ScanOptions {
    ScanOptions {
        cache_path: cache,
        threads: 2,
        ..ScanOptions::default()
    }
}

fn entry<'a>(index: &'a LibraryIndex, name: &str) -> &'a LibraryEntry {
    index
        .entries
        .iter()
        .find(|e| e.file_name() == name)
        .unwrap_or_else(|| panic!("no entry {name}"))
}

mod names {
    use super::*;

    #[test]
    fn bpm_tokens() {
        let cases = [
            ("120bpm_funk", Some(120.0)),
            ("Funk Loop 128 BPM", Some(128.0)),
            ("loop_bpm90_dry", Some(90.0)),
            ("Beat-174.5BPM", Some(174.5)),
            ("loop 95 bpm", Some(95.0)),
            ("bpm 140 loop", Some(140.0)),
            ("kick_01", None),
            ("999bpm", None),
            ("10bpm", None),
            ("synth bpm", None),
        ];
        for (name, bpm) in cases {
            assert_eq!(parse_name(name).bpm, bpm, "{name}");
        }
    }

    #[test]
    fn key_tokens() {
        let key = |tonic, minor| Some(LibraryKey { tonic, minor });
        let cases = [
            ("Am_bass", key(9, true)),
            ("pad_C#m_120", key(1, true)),
            ("lead Ebmaj", key(3, false)),
            ("F#min arp", key(6, true)),
            ("keys_A_minor", key(9, true)),
            ("keys Bb major", key(10, false)),
            ("Gmajor", key(7, false)),
            ("Bbm", key(10, true)),
            // Words that merely start with a note letter, and bare letters, are not keys.
            ("Bass", None),
            ("Drums", None),
            ("Dark", None),
            ("A", None),
            ("am", None),
            ("AM_pad", None),
        ];
        for (name, expected) in cases {
            assert_eq!(parse_name(name).key, expected, "{name}");
        }
        assert_eq!(LibraryKey::parse("Dbm").unwrap().short_name(), "C#m");
        assert_eq!(LibraryKey::parse("A").unwrap().short_name(), "A");
        assert!(LibraryKey::parse("H").is_none());
    }

    #[test]
    fn words_exclude_tokens_and_numbers() {
        let name = parse_name("Dark_Techno_Am_128bpm_01");
        assert_eq!(name.words, vec!["dark", "techno"]);
    }

    #[test]
    fn tags_come_from_folders_and_file_names() {
        let tags = path_tags(
            Path::new("/lib/Packs"),
            Path::new("/lib/Packs/Drums/One Shots/Kick_Heavy_01.wav"),
        );
        assert_eq!(
            tags,
            vec!["drums", "heavy", "kick", "one", "packs", "shots"]
        );
    }
}

mod scan {
    use super::*;

    #[test]
    fn scan_indexes_folders_recursively_with_metadata_and_tags() {
        let dir = tempfile::tempdir().unwrap();
        make_library(dir.path());
        let report = scan(&[dir.path().to_path_buf()], &options(None));
        let index = &report.index;
        assert_eq!(
            index.entries.len(),
            5,
            "txt is ignored, broken.wav is listed"
        );
        assert_eq!((report.analysed, report.cached, report.failed), (5, 0, 1));
        let paths: Vec<_> = index.entries.iter().map(|e| e.path.clone()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "sorted by path");

        let house = entry(index, "loop_a.wav");
        assert_eq!((house.sample_rate, house.channels), (RATE, 1));
        assert!((house.duration_seconds - 12.0).abs() < 0.01);
        let bpm = house.bpm_detected.expect("detected tempo");
        assert!((bpm - 120.0).abs() < 1.0, "detected {bpm}");
        assert_eq!(house.bpm_tag, None);
        assert!(house.tags.contains(&"house".to_string()));
        assert!(house.tags.contains(&"loop".to_string()));

        let bass = entry(index, "Am_dark_bass.wav");
        assert_eq!(bass.channels, 2);
        assert_eq!(
            bass.key_tag,
            Some(LibraryKey {
                tonic: 9,
                minor: true
            })
        );
        assert_eq!(
            bass.key_detected,
            Some(LibraryKey {
                tonic: 9,
                minor: true
            }),
            "detected key of the chord loop"
        );
        assert_eq!(bass.bpm_tag, None);

        let kick = entry(index, "Kick_01.wav");
        assert_eq!((kick.sample_rate, kick.bpm_detected), (48_000, None));
        assert!(kick.tags.contains(&"one".to_string()) && kick.tags.contains(&"kick".to_string()));

        let boom = entry(index, "90bpm_boom_bap.wav");
        assert_eq!(boom.bpm_tag, Some(90.0));
        assert_eq!(boom.bpm(), Some(90.0));
        assert!((boom.bpm_detected.unwrap() - 90.0).abs() < 1.0);

        let broken = entry(index, "broken.wav");
        assert!(broken.error.is_some());
        assert_eq!(broken.duration_seconds, 0.0);
    }

    #[test]
    fn scan_cache_skips_unchanged_files_and_notices_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        make_library(&root);
        let cache = dir.path().join("cache/library.json");
        let roots = [root.clone()];

        let first = scan(&roots, &options(Some(cache.clone())));
        assert_eq!((first.analysed, first.cached), (5, 0));
        assert!(cache.exists());

        // Nothing changed: everything, including the undecodable file, comes from the cache.
        let second = scan(&roots, &options(Some(cache.clone())));
        assert_eq!((second.analysed, second.cached), (0, 5));
        assert_eq!(second.index, first.index);

        // A changed file (different size) is analysed again; the rest stays cached.
        write_wav(
            &root.join("House/loop_a.wav"),
            &[drum_loop(140.0, 9.0)],
            RATE,
        );
        let third = scan(&roots, &options(Some(cache.clone())));
        assert_eq!((third.analysed, third.cached), (1, 4));
        let bpm = entry(&third.index, "loop_a.wav").bpm_detected.unwrap();
        assert!((bpm - 140.0).abs() < 1.0, "re-detected {bpm}");

        // Removed and added files.
        fs::remove_file(root.join("Misc/broken.wav")).unwrap();
        write_wav(&root.join("New/pad.wav"), &[drum_loop(100.0, 6.0)], RATE);
        let fourth = scan(&roots, &options(Some(cache.clone())));
        assert_eq!(fourth.index.entries.len(), 5);
        assert_eq!((fourth.analysed, fourth.cached), (1, 4));
        assert!(
            fourth
                .index
                .entries
                .iter()
                .all(|e| e.file_name() != "broken.wav")
        );

        // `force` ignores the cache.
        let forced = scan(
            &roots,
            &ScanOptions {
                force: true,
                ..options(Some(cache.clone()))
            },
        );
        assert_eq!((forced.analysed, forced.cached), (5, 0));
    }

    #[test]
    fn scan_cache_is_keyed_by_size_limit_and_analysis_version() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        write_wav(&root.join("a.wav"), &[drum_loop(120.0, 6.0)], RATE);
        let cache = dir.path().join("library.json");
        let roots = [root];

        // Too large under a tiny limit: listed with an error, no analysis.
        let tiny = ScanOptions {
            max_file_bytes: 1000,
            ..options(Some(cache.clone()))
        };
        let first = scan(&roots, &tiny);
        assert_eq!(first.failed, 1);
        assert!(first.index.entries[0].bpm_detected.is_none());
        assert_eq!(scan(&roots, &tiny).cached, 1);
        // Raising the limit analyses the file again.
        let raised = scan(&roots, &options(Some(cache.clone())));
        assert_eq!((raised.analysed, raised.failed), (1, 0));
        assert!(raised.index.entries[0].bpm_detected.is_some());
        // An entry from another analysis version is not trusted.
        let mut stale: LibraryIndex = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        stale.entries[0].analysis_version = ANALYSIS_VERSION + 1;
        fs::write(&cache, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert_eq!(scan(&roots, &options(Some(cache))).analysed, 1);
    }

    #[test]
    fn scan_cache_with_another_version_or_bad_json_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        write_wav(&root.join("a.wav"), &[drum_loop(120.0, 6.0)], RATE);
        let cache = dir.path().join("library.json");
        let roots = [root];

        scan(&roots, &options(Some(cache.clone())));
        let mut stale: LibraryIndex = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        stale.version = INDEX_VERSION + 1;
        fs::write(&cache, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert_eq!(scan(&roots, &options(Some(cache.clone()))).analysed, 1);

        fs::write(&cache, b"{ not json").unwrap();
        let again = scan(&roots, &options(Some(cache.clone())));
        assert_eq!((again.analysed, again.index.entries.len()), (1, 1));
        // The damaged cache was replaced by a good one.
        assert_eq!(scan(&roots, &options(Some(cache))).cached, 1);
    }

    #[test]
    fn scan_tags_follow_the_folder_names_even_when_cached() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        write_wav(&root.join("Old/a.wav"), &[drum_loop(120.0, 6.0)], RATE);
        let cache = dir.path().join("library.json");
        let roots = [root.clone()];
        let first = scan(&roots, &options(Some(cache.clone())));
        assert!(first.index.entries[0].tags.contains(&"old".to_string()));
        // Moving a folder changes the path, so the entry is new; the tag follows the name.
        fs::rename(root.join("Old"), root.join("Fresh")).unwrap();
        let second = scan(&roots, &options(Some(cache)));
        assert!(second.index.entries[0].tags.contains(&"fresh".to_string()));
        assert!(!second.index.entries[0].tags.contains(&"old".to_string()));
    }

    #[test]
    fn scan_of_missing_or_empty_roots_gives_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let report = scan(
            &[dir.path().join("nope"), dir.path().to_path_buf()],
            &options(None),
        );
        assert!(report.index.entries.is_empty());
        assert_eq!(report.index.version, INDEX_VERSION);
    }
}

mod search {
    use super::*;

    fn library() -> (tempfile::TempDir, LibraryIndex) {
        let dir = tempfile::tempdir().unwrap();
        make_library(dir.path());
        let index = scan(&[dir.path().to_path_buf()], &options(None)).index;
        (dir, index)
    }

    fn names(results: Vec<&LibraryEntry>) -> Vec<String> {
        results.iter().map(|e| e.file_name()).collect()
    }

    #[test]
    fn search_by_text_and_tags() {
        let (_dir, index) = library();
        assert_eq!(index.search(&Query::default()).len(), 5);
        let text = |t: &str| Query {
            text: t.into(),
            ..Query::default()
        };
        assert_eq!(names(index.search(&text("kick"))), ["Kick_01.wav"]);
        assert_eq!(names(index.search(&text("KICK shots"))), ["Kick_01.wav"]);
        assert_eq!(names(index.search(&text("boom"))), ["90bpm_boom_bap.wav"]);
        assert!(index.search(&text("nothing like this")).is_empty());
        let tagged = Query {
            tags: vec!["Drums".into(), "loops".into()],
            ..Query::default()
        };
        assert_eq!(names(index.search(&tagged)), ["90bpm_boom_bap.wav"]);
    }

    #[test]
    fn search_by_tempo_range_and_key() {
        let (_dir, index) = library();
        let range = |min, max, octaves| Query {
            bpm_min: Some(min),
            bpm_max: Some(max),
            include_half_double: octaves,
            ..Query::default()
        };
        assert_eq!(
            names(index.search(&range(115.0, 125.0, false))),
            ["loop_a.wav"]
        );
        assert_eq!(
            names(index.search(&range(85.0, 95.0, false))),
            ["90bpm_boom_bap.wav"]
        );
        // 58 to 62 only matches through half time: 120 / 2 = 60; the 90 loop's double is 180.
        assert!(index.search(&range(58.0, 62.0, false)).is_empty());
        assert_eq!(
            names(index.search(&range(58.0, 62.0, true))),
            ["loop_a.wav"]
        );
        // Only a lower bound: files without a tempo never match a tempo filter.
        let found = index.search(&Query {
            bpm_min: Some(50.0),
            ..Query::default()
        });
        assert!(found.iter().all(|e| e.bpm().is_some()));
        assert!(!names(found).contains(&"Kick_01.wav".to_string()));

        let key = Query {
            key: LibraryKey::parse("Am"),
            ..Query::default()
        };
        assert_eq!(names(index.search(&key)), ["Am_dark_bass.wav"]);
    }

    #[test]
    fn search_combines_criteria_and_find_looks_up_by_path() {
        let (_dir, index) = library();
        let query = Query {
            text: "loop".into(),
            bpm_min: Some(80.0),
            bpm_max: Some(100.0),
            ..Query::default()
        };
        assert_eq!(names(index.search(&query)), ["90bpm_boom_bap.wav"]);
        let path = index.entries[0].path.clone();
        assert_eq!(index.find(&path), Some(&index.entries[0]));
        assert_eq!(index.find(Path::new("/no/such/file.wav")), None);
    }
}

mod preview {
    use super::*;

    #[test]
    fn preview_in_tempo_length_follows_the_tempo_ratio() {
        let mono = drum_loop(120.0, 8.0);
        let pcm = Pcm {
            sample_rate: RATE,
            channels: vec![mono.clone(), mono],
        };
        // 120 -> 90 BPM: the loop gets a third longer.
        let slower = preview_in_tempo(&pcm, 120.0, 90.0, 0.0).unwrap();
        assert_eq!(slower.channels.len(), 2);
        assert_eq!(
            slower.frames(),
            warp_target_frames(pcm.frames(), 120.0, 90.0)
        );
        assert!((slower.frames() as f64 / pcm.frames() as f64 - 120.0 / 90.0).abs() < 1e-4);
        assert!(slower.channels[0].iter().all(|s| s.is_finite()));
        // 120 -> 150: shorter.
        let faster = preview_in_tempo(&pcm, 120.0, 150.0, 2.0).unwrap();
        assert_eq!(
            faster.frames(),
            warp_target_frames(pcm.frames(), 120.0, 150.0)
        );
        assert_eq!(faster.sample_rate, RATE);
        // The stretched loop is detected at the new tempo.
        let views = [slower.channels[0].as_slice()];
        let tempo = ow_dsp::analysis::detect_tempo(views[0], RATE).unwrap();
        assert!(
            (tempo.bpm - 90.0).abs() < 1.5,
            "previewed loop reads {}",
            tempo.bpm
        );
    }

    #[test]
    fn preview_rejects_bad_tempi() {
        let pcm = Pcm {
            sample_rate: RATE,
            channels: vec![vec![0.0; 1000]],
        };
        assert!(matches!(
            preview_in_tempo(&pcm, 0.0, 120.0, 0.0),
            Err(PreviewError::BadTempo)
        ));
        assert!(matches!(
            preview_in_tempo(&pcm, 120.0, f64::NAN, 0.0),
            Err(PreviewError::BadTempo)
        ));
        assert!(matches!(
            preview_in_tempo(&pcm, 120.0, 20.0, 0.0),
            Err(PreviewError::RatioOutOfRange)
        ));
    }

    #[test]
    fn preview_entry_decodes_the_file_and_needs_a_tempo() {
        let dir = tempfile::tempdir().unwrap();
        make_library(dir.path());
        let index = scan(&[dir.path().to_path_buf()], &options(None)).index;
        let boom = entry(&index, "90bpm_boom_bap.wav");
        let pcm = preview_entry(boom, 120.0, 0.0).unwrap();
        let frames = (10.0 * f64::from(RATE)) as usize;
        assert_eq!(pcm.frames(), warp_target_frames(frames, 90.0, 120.0));
        let kick = entry(&index, "Kick_01.wav");
        assert!(matches!(
            preview_entry(kick, 120.0, 0.0),
            Err(PreviewError::NoTempo)
        ));
        let broken = LibraryEntry {
            bpm_tag: Some(100.0),
            ..entry(&index, "broken.wav").clone()
        };
        assert!(matches!(
            preview_entry(&broken, 120.0, 0.0),
            Err(PreviewError::Decode(_))
        ));
    }
}
