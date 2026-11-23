//! Finding plugin files (CLAP and VST3) and scanning them out of process.
//!
//! A scan loads each candidate in a child process (`ow-scan <file>`, or the app's own
//! `openwave-studio --scan-plugin <file>`): loading a plugin and instantiating it to read its
//! ports and parameters runs the vendor's code, and a plugin that crashes or hangs must not take
//! the app down. The child prints one result line, [`RESULT_MARKER`] followed by JSON
//! ([`ScanOutput`]); anything else on its output (plugins print things) is ignored. A child that
//! dies, prints no result or runs past the timeout (10 s by default) marks the file `crashed` or
//! `timed_out`; the record is cached by path, size and modification time, so the file is not
//! tried again until it changes (or a forced rescan).

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::executor::{InlineExecutor, MainThreadExecutor};
use crate::info::{
    FileRecord, Format, PluginInfo, RECORD_VERSION, ScanCache, ScanOutput, ScanStatus,
};
use crate::{clap, vst3};

/// The scanner's timeout per file.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the scanner waits for a finished child's output pipe to close.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// What precedes the JSON result on the child's standard output.
pub const RESULT_MARKER: &str = "OW-SCAN-RESULT ";

/// How deep below a scan folder plugin files are looked for (vendors use sub-folders).
const MAX_DEPTH: usize = 4;

/// Most plugin files one scan looks at.
const MAX_CANDIDATES: usize = 2000;

/// Runs one scan in the current process and prints the result line: the body of `ow-scan` and
/// of `openwave-studio --scan-plugin`. Returns the process exit code (0 when the file loaded).
/// A crashing plugin ends the process before anything is printed, which is what the parent's
/// `crashed` status means.
pub fn run_child(path: &Path) -> i32 {
    let (output, code) = match scan_in_process(path) {
        Ok(plugins) => (
            ScanOutput::Plugins {
                version: RECORD_VERSION,
                plugins,
            },
            0,
        ),
        Err(error) => (ScanOutput::Error { error }, 2),
    };
    let line = serde_json::to_string(&output).unwrap_or_else(|_| r#"{"error":"encode"}"#.into());
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "\n{RESULT_MARKER}{line}");
    let _ = stdout.flush();
    code
}

/// Scans `path` in this process and returns the plugins (what `run_child` prints). The format
/// follows the file's extension (`.clap` or `.vst3`). A crashing plugin crashes the caller:
/// tests and tools that need isolation use [`Scanner`].
pub fn scan_in_process(path: &Path) -> Result<Vec<PluginInfo>, String> {
    let executor: Arc<dyn MainThreadExecutor> = Arc::new(InlineExecutor::new());
    match format_of(path) {
        Some(Format::Vst3) => vst3::describe_file(&executor, path),
        Some(Format::Clap) => clap::describe_file(&executor, path),
        None => Err("not a CLAP or VST3 plugin file (expected .clap or .vst3)".into()),
    }
}

/// The plugin format a file name's extension stands for.
pub fn format_of(path: &Path) -> Option<Format> {
    let extension = path.extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("clap") {
        Some(Format::Clap)
    } else if extension.eq_ignore_ascii_case("vst3") {
        Some(Format::Vst3)
    } else {
        None
    }
}

/// How to start the scanner child: `program args... <plugin file>`.
#[derive(Debug, Clone)]
pub struct ScanCommand {
    /// The executable (`ow-scan`, or the app itself).
    pub program: PathBuf,
    /// Arguments before the plugin path (`--scan-plugin` for the app).
    pub args: Vec<OsString>,
}

impl ScanCommand {
    /// A command that runs `program` with the plugin path as its only argument.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
        }
    }

    /// A command with arguments before the plugin path.
    pub fn with_args(program: impl Into<PathBuf>, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(OsString::from).collect(),
        }
    }
}

/// Scans plugin files in child processes and keeps the results in a cache file.
#[derive(Debug, Clone)]
pub struct Scanner {
    command: ScanCommand,
    timeout: Duration,
    env: Vec<(OsString, OsString)>,
    cache_path: Option<PathBuf>,
}

/// What one scan did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanReport {
    /// A record for every plugin file found, in path order.
    pub files: Vec<FileRecord>,
    /// How many files a child was started for.
    pub scanned: usize,
    /// How many were answered from the cache.
    pub cached: usize,
}

impl ScanReport {
    /// Every plugin found, in file order.
    pub fn plugins(&self) -> Vec<PluginInfo> {
        self.files
            .iter()
            .filter(|file| file.status.is_ok())
            .flat_map(|file| file.plugins.iter().cloned())
            .collect()
    }

    /// The files that crashed, timed out or failed to load.
    pub fn failures(&self) -> Vec<&FileRecord> {
        self.files.iter().filter(|f| !f.status.is_ok()).collect()
    }
}

impl Scanner {
    /// A scanner that starts `command` for each file, with a 10 second timeout.
    pub fn new(command: ScanCommand) -> Self {
        Self {
            command,
            timeout: DEFAULT_TIMEOUT,
            env: Vec::new(),
            cache_path: None,
        }
    }

    /// Changes the per-file timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets an environment variable for the child (tests use it to make a fixture hang).
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Keeps results in the JSON cache file at `path`.
    pub fn with_cache(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_path = Some(path.into());
        self
    }

    /// Scans one file in a child process; always yields a record, whatever happened to the
    /// child.
    pub fn scan_file(&self, path: &Path) -> FileRecord {
        let (size, modified_ms) = fingerprint(path).unwrap_or((0, 0));
        let (status, plugins) = self.run_child(path);
        FileRecord {
            path: path.to_string_lossy().into_owned(),
            size,
            modified_ms,
            status,
            plugins,
        }
    }

    fn run_child(&self, path: &Path) -> (ScanStatus, Vec<PluginInfo>) {
        let mut command = Command::new(&self.command.program);
        command
            .args(&self.command.args)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in &self.env {
            command.env(key, value);
        }
        #[cfg(windows)]
        {
            // No console window per child.
            std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return (
                    ScanStatus::Failed {
                        message: format!(
                            "could not start the scanner {}: {error}",
                            self.command.program.display()
                        ),
                    },
                    Vec::new(),
                );
            }
        };
        // Read the pipe on its own thread so a chatty plugin cannot fill it and stall. The thread
        // is never joined: a helper process the plugin started may inherit the pipe and keep it
        // open long after the child itself is gone, and waiting for its end would hang the scan.
        let mut stdout = child.stdout.take();
        let (answer, bytes_arrive) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(stdout) = stdout.as_mut() {
                let _ = stdout.take(4 << 20).read_to_end(&mut bytes);
            }
            let _ = answer.send(bytes);
        });
        let deadline = Instant::now() + self.timeout;
        let exit = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let Some(exit) = exit else {
            return (ScanStatus::TimedOut, Vec::new());
        };
        // The child is gone, so its output is complete unless a helper still holds the pipe: give
        // the reader a moment, then judge by what it has not delivered (nothing).
        let bytes = bytes_arrive.recv_timeout(PIPE_GRACE).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        let result = text
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix(RESULT_MARKER))
            .and_then(|json| serde_json::from_str::<ScanOutput>(json).ok());
        match result {
            Some(ScanOutput::Plugins { plugins, .. }) => (ScanStatus::Ok, plugins),
            Some(ScanOutput::Error { error }) => {
                (ScanStatus::Failed { message: error }, Vec::new())
            }
            None => (
                ScanStatus::Crashed {
                    detail: format!("the scanner ended without a result ({exit})"),
                },
                Vec::new(),
            ),
        }
    }

    /// Scans every plugin file under `roots`, reusing cached records for files that did not
    /// change (unless `force`), and saves the cache. `roots` may also name plugin files
    /// directly.
    pub fn scan(&self, roots: &[PathBuf], force: bool) -> ScanReport {
        let cache = self.load_cache();
        let mut report = ScanReport::default();
        for path in candidates(roots) {
            let text = path.to_string_lossy().into_owned();
            let print = fingerprint(&path);
            if !force
                && let Some((size, modified)) = print
                && let Some(record) = cache.fresh(&text, size, modified)
            {
                report.files.push(record.clone());
                report.cached += 1;
                continue;
            }
            report.files.push(self.scan_file(&path));
            report.scanned += 1;
        }
        self.save_cache(&ScanCache {
            version: RECORD_VERSION,
            files: report.files.clone(),
        });
        report
    }

    /// The cached records, without scanning anything.
    pub fn load_cache(&self) -> ScanCache {
        self.cache_path
            .as_ref()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<ScanCache>(&bytes).ok())
            .filter(|cache| cache.version == RECORD_VERSION)
            .unwrap_or_default()
    }

    fn save_cache(&self, cache: &ScanCache) {
        let Some(path) = &self.cache_path else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec_pretty(cache) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut temp = path.as_os_str().to_owned();
        temp.push(".writing");
        let temp = PathBuf::from(temp);
        if fs::write(&temp, bytes).is_ok() && fs::rename(&temp, path).is_err() {
            let _ = fs::remove_file(&temp);
        }
    }
}

/// Size and modification time (milliseconds since the epoch) of a plugin file; for a macOS
/// bundle, of its executable.
pub fn fingerprint(path: &Path) -> Option<(u64, u64)> {
    let file = executable_of(path);
    let meta = fs::metadata(file.as_deref().unwrap_or(path)).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some((meta.len(), u64::try_from(modified).ok()?))
}

/// The executable inside a plugin bundle folder, when `path` is one: `Contents/MacOS/*` on
/// macOS, `Contents/<arch>-win/*` and `Contents/<arch>-linux/*` for VST3 bundles elsewhere.
pub(crate) fn executable_of(path: &Path) -> Option<PathBuf> {
    if !path.is_dir() {
        return None;
    }
    let contents = path.join("Contents");
    let folders: &[&str] = if cfg!(target_os = "macos") {
        &["MacOS"]
    } else if cfg!(windows) {
        &["x86_64-win", "arm64-win", "arm64ec-win"]
    } else if cfg!(target_arch = "aarch64") {
        &["aarch64-linux", "x86_64-linux"]
    } else {
        &["x86_64-linux", "aarch64-linux"]
    };
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    for folder in folders {
        let dir = contents.join(folder);
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = entries
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| path.is_file())
            .collect();
        entries.sort();
        // Prefer the file named like the bundle (`Name.vst3/Contents/x86_64-win/Name.vst3`).
        let named = entries
            .iter()
            .find(|entry| entry.file_stem().and_then(|s| s.to_str()) == stem)
            .cloned();
        if let Some(found) = named.or_else(|| entries.into_iter().next()) {
            return Some(found);
        }
    }
    None
}

fn is_plugin_name(path: &Path) -> bool {
    format_of(path).is_some()
}

/// The plugin files under `roots`: files or bundle folders named `*.clap` or `*.vst3`, found up to
/// [`MAX_DEPTH`] folders down, in path order. A root that is itself a plugin file counts.
pub fn candidates(roots: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<PathBuf> = entries.filter_map(|e| Some(e.ok()?.path())).collect();
        entries.sort();
        for path in entries {
            if found.len() >= MAX_CANDIDATES {
                return;
            }
            if is_plugin_name(&path) {
                found.push(path);
            } else if path.is_dir() && depth < MAX_DEPTH {
                walk(&path, depth + 1, found);
            }
        }
    }
    let mut found = Vec::new();
    for root in roots {
        // Absolute, so the path a record stores still means the same file after the working
        // directory changes.
        let root = std::path::absolute(root).unwrap_or_else(|_| root.clone());
        if is_plugin_name(&root) && root.exists() {
            found.push(root);
        } else {
            walk(&root, 0, &mut found);
        }
    }
    found.sort();
    found.dedup();
    found
}

/// The folders CLAP and VST3 plugins are installed in on this OS, plus those of `CLAP_PATH` and
/// `VST3_PATH`.
pub fn default_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    #[cfg(windows)]
    {
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            paths.push(PathBuf::from(&common).join("CLAP"));
            paths.push(PathBuf::from(common).join("VST3"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let common = PathBuf::from(local).join("Programs").join("Common");
            paths.push(common.join("CLAP"));
            paths.push(common.join("VST3"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
        if let Some(home) = std::env::var_os("HOME") {
            let plug_ins = PathBuf::from(home).join("Library/Audio/Plug-Ins");
            paths.push(plug_ins.join("CLAP"));
            paths.push(plug_ins.join("VST3"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        paths.push(PathBuf::from("/usr/lib/clap"));
        paths.push(PathBuf::from("/usr/lib/vst3"));
        paths.push(PathBuf::from("/usr/local/lib/vst3"));
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            paths.push(home.join(".clap"));
            paths.push(home.join(".vst3"));
        }
    }
    for variable in ["CLAP_PATH", "VST3_PATH"] {
        if let Some(extra) = std::env::var_os(variable) {
            paths.extend(std::env::split_paths(&extra));
        }
    }
    paths.retain(|path| !path.as_os_str().is_empty());
    paths.dedup();
    paths
}

/// Seconds since the epoch, for tests that age cache records.
#[doc(hidden)]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_find_plugin_files_in_nested_folders_and_skip_the_rest() {
        let dir = std::env::temp_dir().join(format!("ow-scan-candidates-{}", now_ms()));
        let vendor = dir.join("Vendor");
        fs::create_dir_all(&vendor).unwrap();
        fs::write(dir.join("a.clap"), b"x").unwrap();
        fs::write(vendor.join("b.CLAP"), b"x").unwrap();
        // A VST3 bundle folder is one candidate; what is inside it is not looked at.
        let bundle = vendor.join("c.vst3");
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        fs::write(bundle.join("Contents").join("inner.clap"), b"x").unwrap();
        fs::write(vendor.join("notes.txt"), b"x").unwrap();
        let found = candidates(std::slice::from_ref(&dir));
        let mut names: Vec<_> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.clap", "b.CLAP", "c.vst3"]);
        // A plugin file named directly is a candidate too; a missing root is nothing.
        assert_eq!(candidates(&[dir.join("a.clap")]).len(), 1);
        assert!(candidates(&[dir.join("missing")]).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_scanner_that_cannot_start_reports_a_failed_record() {
        let scanner = Scanner::new(ScanCommand::new("this-program-does-not-exist"));
        let record = scanner.scan_file(Path::new("nothing.clap"));
        assert!(
            matches!(record.status, ScanStatus::Failed { .. }),
            "{record:?}"
        );
    }
}
