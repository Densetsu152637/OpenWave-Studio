//! Self-update of a Windows portable folder.
//!
//! The installer-based updater cannot update a folder that holds a plain executable, so the
//! portable zip is updated here. The running app checks the feed with the custom target
//! `windows-<arch>-portable`, downloads the zip (the updater plugin verifies its minisign
//! signature) and extracts it to `<folder>/.update-staging/`. When the app exits, a copy of the
//! executable is started from the temp directory with `--apply-update`; `main()` hands that flag
//! to [`handle_args`] before Tauri initializes. The helper waits for the app to exit and swaps the
//! files:
//!
//! 1. every top-level entry of the folder except `OpenWave Studio Data/`, the `portable` marker and
//!    `.update-staging/` is renamed to `<name>.ow-old`; the program itself goes last, and its
//!    replacement is moved in right after, so the folder lacks the executable for one rename only;
//! 2. every staged entry is moved in (staged entries named `portable` or `OpenWave Studio Data`
//!    are skipped, so a malformed package cannot overwrite them);
//! 3. on any error every moved-in entry is removed and every `.ow-old` is renamed back, which leaves
//!    the old version intact. On success the new version is started first, then the `.ow-old`
//!    entries and the staging folder are deleted.
//!
//! Each rename is retried with exponential backoff, starting at 100 ms, while the error is a
//! transient lock (antivirus, search indexer) and a shared 30 s budget lasts. A failed update
//! leaves `update-failed.txt` in the data folder's `logs`, which the app shows at its next start.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::paths::{DATA_DIR_NAME, MARKER_FILE};

/// Flag that turns a copy of the executable into the swap helper.
pub const APPLY_FLAG: &str = "--apply-update";
/// Staging folder inside the install folder; never part of the swap.
pub const STAGING_DIR: &str = ".update-staging";
/// The executable inside the portable zip, and so inside an updated folder.
pub const PORTABLE_EXE: &str = "OpenWave Studio.exe";
/// Suffix of a renamed old entry while the swap runs. Distinctive, so a `<name>.bak` the user
/// keeps is never mistaken for ours.
const BACKUP_SUFFIX: &str = ".ow-old";
/// Total time the helper spends on waiting for the app and on locked files.
pub const SWAP_BUDGET: Duration = Duration::from_secs(30);
/// First wait between attempts; it doubles after each failure.
pub const FIRST_BACKOFF: Duration = Duration::from_millis(100);
/// Log file name, written under the data folder's `logs`.
const LOG_FILE: &str = "portable-update.log";
/// Left in the same folder by a failed update; read and deleted by the next app start.
const FAILURE_FILE: &str = "update-failed.txt";

/// The updater feed key for a portable zip of this architecture, if one is published.
pub fn feed_target(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("windows-x86_64-portable"),
        "aarch64" => Some("windows-aarch64-portable"),
        _ => None,
    }
}

/// Names the swap never touches in the old folder and never moves in from a package. Windows
/// ignores case and trailing dots and spaces in names, so `Portable.` is the marker too.
fn is_protected(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    let name = name.trim_end_matches(['.', ' ']);
    [DATA_DIR_NAME, MARKER_FILE, STAGING_DIR]
        .iter()
        .any(|protected| name.eq_ignore_ascii_case(protected))
}

fn is_program(name: &OsStr) -> bool {
    name.to_string_lossy().eq_ignore_ascii_case(PORTABLE_EXE)
}

/// One rename of the swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// What a swap will do, computed before anything is touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapPlan {
    /// Old entries renamed to `.ow-old`.
    pub backups: Vec<Step>,
    /// Staged entries moved into the folder.
    pub installs: Vec<Step>,
}

/// One operation of the swap, in execution order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op<'a> {
    Backup(&'a Step),
    Install(&'a Step),
}

impl SwapPlan {
    /// Everything but the program first, then the program's backup and its replacement, so a
    /// crash can only strand the folder between those two adjacent renames.
    pub fn ordered(&self) -> Vec<Op<'_>> {
        let backs_up_exe = |step: &&Step| is_program(step.from.file_name().unwrap_or_default());
        let installs_exe = |step: &&Step| is_program(step.to.file_name().unwrap_or_default());
        let mut ops = Vec::new();
        ops.extend(
            self.backups
                .iter()
                .filter(|s| !backs_up_exe(s))
                .map(Op::Backup),
        );
        ops.extend(
            self.installs
                .iter()
                .filter(|s| !installs_exe(s))
                .map(Op::Install),
        );
        ops.extend(self.backups.iter().filter(backs_up_exe).map(Op::Backup));
        ops.extend(self.installs.iter().filter(installs_exe).map(Op::Install));
        ops
    }
}

fn top_level_names(dir: &Path) -> io::Result<Vec<OsString>> {
    let mut names = fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}

/// Plans the swap of `staging`'s entries into `install`. Fails when the package has nothing to
/// install, so an empty download can never strip the folder.
pub fn plan_swap(install: &Path, staging: &Path) -> io::Result<SwapPlan> {
    let backups = top_level_names(install)?
        .into_iter()
        // A leftover `.ow-old` from an interrupted update is not part of the old version.
        .filter(|name| !is_protected(name) && !name.to_string_lossy().ends_with(BACKUP_SUFFIX))
        .map(|name| {
            let mut backup = name.clone();
            backup.push(BACKUP_SUFFIX);
            Step {
                from: install.join(&name),
                to: install.join(backup),
            }
        })
        .collect();
    let installs: Vec<Step> = top_level_names(staging)?
        .into_iter()
        .filter(|name| !is_protected(name))
        .map(|name| Step {
            from: staging.join(&name),
            to: install.join(&name),
        })
        .collect();
    if installs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the update package holds no files to install",
        ));
    }
    Ok(SwapPlan { backups, installs })
}

/// The file operations a swap needs; tests inject failures through it.
pub trait Fs {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Removes a file or a whole folder; a missing path is not an error.
    fn remove(&self, path: &Path) -> io::Result<()>;
}

/// The real file system.
pub struct RealFs;

impl Fs for RealFs {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        let result = match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
            Ok(_) => fs::remove_file(path),
            Err(error) => Err(error),
        };
        match result {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// How long an operation may keep retrying.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub total: Duration,
    pub first_backoff: Duration,
}

impl Budget {
    pub const DEFAULT: Self = Self {
        total: SWAP_BUDGET,
        first_backoff: FIRST_BACKOFF,
    };
}

/// Whether waiting could help: a file held open by another process (sharing or lock violation,
/// access denied while a scanner has it) or a momentary interruption. Anything else, such as a
/// missing path or a full disk, fails at once.
fn is_transient(error: &io::Error) -> bool {
    use io::ErrorKind::{Interrupted, PermissionDenied, ResourceBusy, TimedOut, WouldBlock};
    // Windows: 5 access denied, 32 sharing violation, 33 lock violation.
    matches!(error.raw_os_error(), Some(5 | 32 | 33))
        || matches!(
            error.kind(),
            PermissionDenied | ResourceBusy | WouldBlock | Interrupted | TimedOut
        )
}

/// Runs `operation` until it succeeds, fails with a non-transient error, or `deadline` passes,
/// waiting `first_backoff`, then twice as long after every failure (never beyond the deadline).
fn retry<T>(
    deadline: Instant,
    first_backoff: Duration,
    mut operation: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut delay = first_backoff;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                let now = Instant::now();
                if !is_transient(&error) || now >= deadline {
                    return Err(error);
                }
                std::thread::sleep(delay.min(deadline - now));
                delay = delay.saturating_mul(2);
            }
        }
    }
}

/// Why a swap failed, and whether the old version could be restored completely.
#[derive(Debug)]
pub struct SwapError {
    pub cause: io::Error,
    /// Problems met while undoing the swap; empty when the old version is fully back.
    pub restore_problems: Vec<String>,
}

impl std::fmt::Display for SwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.cause)?;
        if !self.restore_problems.is_empty() {
            write!(
                f,
                " (restore incomplete: {})",
                self.restore_problems.join("; ")
            )?;
        }
        Ok(())
    }
}

/// Carries out `plan`, leaving the `.ow-old` backups in place for [`cleanup_backups`]. On any
/// error everything done so far is undone (each undo step is retried within its own budget).
pub fn apply_swap(
    fs: &dyn Fs,
    plan: &SwapPlan,
    budget: Budget,
    deadline: Instant,
) -> Result<(), SwapError> {
    let mut backed_up: Vec<&Step> = Vec::new();
    let mut installed: Vec<&Step> = Vec::new();
    let outcome = (|| {
        for op in plan.ordered() {
            match op {
                Op::Backup(step) => {
                    // A stale backup of an interrupted update would block the rename.
                    retry(deadline, budget.first_backoff, || fs.remove(&step.to))?;
                    retry(deadline, budget.first_backoff, || {
                        fs.rename(&step.from, &step.to)
                    })?;
                    backed_up.push(step);
                }
                Op::Install(step) => {
                    retry(deadline, budget.first_backoff, || {
                        fs.rename(&step.from, &step.to)
                    })?;
                    installed.push(step);
                }
            }
        }
        Ok(())
    })();
    outcome.map_err(|cause| SwapError {
        cause,
        restore_problems: restore(fs, &installed, &backed_up, budget),
    })
}

/// Deletes the backups of a finished swap, best effort, until `deadline`. Returns how many could
/// not be removed (the next update removes them).
pub fn cleanup_backups(fs: &dyn Fs, plan: &SwapPlan, budget: Budget, deadline: Instant) -> usize {
    plan.backups
        .iter()
        .filter(|step| retry(deadline, budget.first_backoff, || fs.remove(&step.to)).is_err())
        .count()
}

/// Removes the moved-in entries and renames every backup back, newest first.
fn restore(fs: &dyn Fs, installed: &[&Step], backed_up: &[&Step], budget: Budget) -> Vec<String> {
    let mut problems = Vec::new();
    for step in installed.iter().rev() {
        let deadline = Instant::now() + budget.total;
        if let Err(error) = retry(deadline, budget.first_backoff, || fs.remove(&step.to)) {
            problems.push(format!("removing {}: {error}", step.to.display()));
        }
    }
    for step in backed_up.iter().rev() {
        let deadline = Instant::now() + budget.total;
        if let Err(error) = retry(deadline, budget.first_backoff, || {
            fs.rename(&step.to, &step.from)
        }) {
            problems.push(format!("restoring {}: {error}", step.from.display()));
        }
    }
    problems
}

/// Replaces the staging folder's content with the files of the downloaded zip. Entries that would
/// land outside the staging folder are refused, and the package must hold the executable.
pub fn stage(install: &Path, zip_bytes: &[u8]) -> io::Result<PathBuf> {
    let staging = install.join(STAGING_DIR);
    RealFs.remove(&staging)?;
    fs::create_dir_all(&staging)?;
    if let Err(error) = extract(zip_bytes, &staging).and_then(|()| check_staged(&staging)) {
        let _ = RealFs.remove(&staging);
        return Err(error);
    }
    Ok(staging)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn extract(zip_bytes: &[u8], into: &Path) -> io::Result<()> {
    let mut archive = zip::ZipArchive::new(io::Cursor::new(zip_bytes))
        .map_err(|error| invalid(format!("the update is not a zip file: {error}")))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| invalid(format!("unreadable zip entry: {error}")))?;
        let relative = entry.enclosed_name().ok_or_else(|| {
            invalid(format!(
                "unsafe path in the update: {}",
                entry.name().unwrap_or_default()
            ))
        })?;
        let target = into.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::File::create(&target)?;
            io::copy(&mut entry, &mut file)?;
        }
    }
    Ok(())
}

fn check_staged(staging: &Path) -> io::Result<()> {
    if top_level_names(staging)?
        .iter()
        .any(|name| is_program(name))
    {
        Ok(())
    } else {
        Err(invalid(format!("the update package has no {PORTABLE_EXE}")))
    }
}

/// Arguments of the swap helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperArgs {
    /// Process to wait for (the app that started the helper).
    pub pid: u32,
    /// The portable folder.
    pub dir: PathBuf,
    /// Start the app again once the swap is done (restart now, as opposed to install on quit).
    pub relaunch: bool,
}

impl HelperArgs {
    /// The command line that starts the helper.
    pub fn to_args(&self) -> Vec<OsString> {
        let mut args = vec![
            OsString::from(APPLY_FLAG),
            OsString::from("--pid"),
            OsString::from(self.pid.to_string()),
            OsString::from("--dir"),
            self.dir.clone().into_os_string(),
        ];
        if self.relaunch {
            args.push(OsString::from("--relaunch"));
        }
        args
    }

    /// `None` when the command line is not a helper invocation; `Some(Err)` when it is one with
    /// bad arguments.
    pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Option<Result<Self, String>> {
        let mut args = args.into_iter();
        let mut is_helper = false;
        let (mut pid, mut dir, mut relaunch) = (None, None, false);
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some(APPLY_FLAG) => is_helper = true,
                Some("--pid") => pid = args.next().and_then(|v| v.to_str()?.parse::<u32>().ok()),
                Some("--dir") => dir = args.next().map(PathBuf::from),
                Some("--relaunch") => relaunch = true,
                _ => {}
            }
        }
        if !is_helper {
            return None;
        }
        Some(match (pid, dir) {
            (Some(pid), Some(dir)) => Ok(Self { pid, dir, relaunch }),
            _ => Err(format!(
                "{APPLY_FLAG} needs --pid <number> and --dir <folder>"
            )),
        })
    }
}

/// Handles `--apply-update` in `main()`. Returns the exit code when the invocation was a helper
/// run, or `None` to continue starting the app.
pub fn handle_args<I: IntoIterator<Item = OsString>>(args: I) -> Option<i32> {
    let parsed = HelperArgs::parse(args)?;
    Some(match parsed {
        Ok(args) => run_helper(&args),
        Err(message) => {
            eprintln!("[openwave] update helper: {message}");
            2
        }
    })
}

/// The `logs` folder inside the portable data folder.
fn logs_dir(dir: &Path) -> PathBuf {
    dir.join(DATA_DIR_NAME).join("logs")
}

/// Appends a line to the helper's log, when the data folder exists.
fn log_line(dir: &Path, message: &str) {
    use std::io::Write;
    let logs = logs_dir(dir);
    if !logs.is_dir() {
        return;
    }
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join(LOG_FILE))
    {
        let _ = writeln!(file, "{message}");
    }
}

/// Records a failed update for the next app start.
pub fn record_failure(dir: &Path, message: &str) {
    let logs = logs_dir(dir);
    if fs::create_dir_all(&logs).is_ok() {
        let _ = fs::write(logs.join(FAILURE_FILE), message);
    }
}

/// The message a failed update left in `logs` (the data folder's log folder), once: the file is
/// deleted by reading it. The text names the log file for details.
pub fn take_failure(logs: &Path) -> Option<String> {
    let file = logs.join(FAILURE_FILE);
    let message = fs::read_to_string(&file).ok()?;
    let _ = fs::remove_file(&file);
    Some(format!(
        "The last update failed and the old version was kept: {} (details: {})",
        message.trim(),
        logs.join(LOG_FILE).display()
    ))
}

/// Waits for the app to exit, swaps the files and optionally starts the app again. Always leaves
/// either the new version or the old one in the folder.
fn run_helper(args: &HelperArgs) -> i32 {
    let deadline = Instant::now() + SWAP_BUDGET;
    log_line(&args.dir, "update helper started");
    if !wait_for_exit(args.pid, SWAP_BUDGET) {
        log_line(
            &args.dir,
            "the app did not exit in time; trying the swap anyway",
        );
    }
    let staging = args.dir.join(STAGING_DIR);
    let plan = plan_swap(&args.dir, &staging);
    let result = match &plan {
        Ok(plan) => apply_swap(&RealFs, plan, Budget::DEFAULT, deadline),
        Err(cause) => Err(SwapError {
            cause: io::Error::new(cause.kind(), cause.to_string()),
            restore_problems: Vec::new(),
        }),
    };
    let code = match &result {
        Ok(()) => {
            log_line(&args.dir, "update applied");
            0
        }
        Err(error) => {
            let message = error.to_string();
            log_line(
                &args.dir,
                &format!("update failed, old version kept: {message}"),
            );
            record_failure(&args.dir, &message);
            1
        }
    };
    // Start the app first, so the user does not wait for the cleanup.
    if args.relaunch {
        let exe = args.dir.join(PORTABLE_EXE);
        match std::process::Command::new(&exe)
            .current_dir(&args.dir)
            .spawn()
        {
            Ok(_) => log_line(&args.dir, "app started again"),
            Err(error) => log_line(
                &args.dir,
                &format!("could not start {}: {error}", exe.display()),
            ),
        }
    }
    if let (Ok(()), Ok(plan)) = (&result, &plan) {
        let left = cleanup_backups(&RealFs, plan, Budget::DEFAULT, deadline);
        if left > 0 {
            log_line(
                &args.dir,
                &format!("{left} backup entries could not be removed"),
            );
        }
    }
    let _ = RealFs.remove(&staging);
    remove_own_copy();
    code
}

/// Deletes this helper's copy in the temp folder once the process is gone (a running exe cannot
/// delete itself on Windows): a detached `cmd` waits a few seconds, then removes it.
#[cfg(windows)]
fn remove_own_copy() {
    use std::os::windows::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new("cmd.exe")
        .args(["/C", "ping -n 4 127.0.0.1 >NUL & del /F /Q"])
        .arg(&exe)
        .creation_flags(0x0000_0008 | 0x0800_0000)
        .spawn();
}

#[cfg(not(windows))]
fn remove_own_copy() {}

#[cfg(windows)]
#[allow(unsafe_code)]
fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: plain Win32 calls; the handle is closed before returning. A process that cannot be
    // opened has already exited (or is not ours to wait for), so there is nothing to wait for.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return true;
        }
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        let exited = WaitForSingleObject(handle, millis) == WAIT_OBJECT_0;
        CloseHandle(handle);
        exited
    }
}

#[cfg(not(windows))]
fn wait_for_exit(_pid: u32, _timeout: Duration) -> bool {
    true
}

/// Starts the swap helper from a copy of this executable in the temp directory (the copy keeps
/// the install folder's exe unlocked) and returns; the caller then exits. The copy is named for
/// this process, and a failed copy is an error: a file left by someone else is never run.
pub fn launch_helper(dir: &Path, relaunch: bool) -> io::Result<()> {
    let pid = std::process::id();
    let helper_dir = std::env::temp_dir().join("openwave-studio-update");
    fs::create_dir_all(&helper_dir)?;
    sweep_old_helpers(&helper_dir);
    let helper = helper_dir.join(format!("openwave-update-helper-{pid}.exe"));
    RealFs.remove(&helper)?;
    fs::copy(std::env::current_exe()?, &helper)?;
    let args = HelperArgs {
        pid,
        dir: dir.to_path_buf(),
        relaunch,
    };
    let mut command = std::process::Command::new(helper);
    command.args(args.to_args());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NO_WINDOW: the helper outlives this process and has no console.
        command.creation_flags(0x0000_0008 | 0x0800_0000);
    }
    command.spawn().map(|_| ())
}

/// Removes helper copies of earlier updates; one still running cannot be deleted and stays.
fn sweep_old_helpers(helper_dir: &Path) {
    if let Ok(entries) = fs::read_dir(helper_dir) {
        for entry in entries.flatten() {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Runs `launch` for the pending `item`; when it fails the item goes back into `slot`, so a later
/// "restart now" or quit can try again.
pub fn launch_or_restore<T>(
    slot: &mut Option<T>,
    item: T,
    launch: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    launch().inspect_err(|_| *slot = Some(item))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    use super::*;

    /// Fast retries so failing tests do not wait.
    const FAST: Budget = Budget {
        total: Duration::from_millis(60),
        first_backoff: Duration::from_millis(1),
    };

    fn deadline() -> Instant {
        Instant::now() + FAST.total
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    /// An install folder of an old version beside a staged new version.
    fn folder() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("OpenWave Studio.exe"), b"old exe");
        write(&root.join("LICENSE"), b"old license");
        write(&root.join("old-only.dll"), b"old dll");
        write(&root.join("res/a.txt"), b"old resource");
        write(&root.join(MARKER_FILE), b"");
        write(
            &root.join(DATA_DIR_NAME).join("config/settings.json"),
            b"{\"volume\":7}",
        );
        let staging = root.join(STAGING_DIR);
        write(&staging.join("OpenWave Studio.exe"), b"new exe");
        write(&staging.join("LICENSE"), b"new license");
        write(&staging.join("res/b.txt"), b"new resource");
        dir
    }

    /// Every file and folder under `dir` (except the staging folder) with its bytes.
    fn snapshot(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if name == STAGING_DIR {
                    continue;
                }
                if path.is_dir() {
                    out.insert(name, None);
                    walk(root, &path, out);
                } else {
                    out.insert(name, Some(fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    fn plan_for(dir: &Path) -> SwapPlan {
        plan_swap(dir, &dir.join(STAGING_DIR)).unwrap()
    }

    #[test]
    fn portable_update_plan_backs_up_old_entries_and_installs_staged_ones() {
        let dir = folder();
        let root = dir.path();
        let plan = plan_for(root);
        let backed_up: Vec<_> = plan
            .backups
            .iter()
            .map(|s| s.from.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            backed_up,
            ["LICENSE", "OpenWave Studio.exe", "old-only.dll", "res"]
        );
        for step in &plan.backups {
            assert_eq!(
                step.to.file_name().unwrap().to_string_lossy(),
                format!(
                    "{}.ow-old",
                    step.from.file_name().unwrap().to_string_lossy()
                )
            );
        }
        let installed: Vec<_> = plan
            .installs
            .iter()
            .map(|s| s.to.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(installed, ["LICENSE", "OpenWave Studio.exe", "res"]);
    }

    #[test]
    fn portable_update_plan_never_touches_data_marker_or_staging() {
        let dir = folder();
        let plan = plan_for(dir.path());
        let staging = dir.path().join(STAGING_DIR);
        for step in &plan.backups {
            assert!(!is_protected(step.from.file_name().unwrap()), "{step:?}");
        }
        for step in &plan.installs {
            assert!(step.from.starts_with(&staging), "{step:?}");
            assert!(!is_protected(step.to.file_name().unwrap()), "{step:?}");
        }
        // The protection is case-insensitive, like the Windows file system.
        assert!(is_protected(OsStr::new("PORTABLE")));
        assert!(is_protected(OsStr::new("openwave studio data")));
    }

    #[test]
    fn portable_update_swap_replaces_the_program_and_keeps_user_data() {
        let dir = folder();
        let root = dir.path();
        let plan = plan_for(root);
        apply_swap(&RealFs, &plan, FAST, deadline()).unwrap();
        assert_eq!(cleanup_backups(&RealFs, &plan, FAST, deadline()), 0);
        assert_eq!(
            fs::read(root.join("OpenWave Studio.exe")).unwrap(),
            b"new exe"
        );
        assert_eq!(fs::read(root.join("LICENSE")).unwrap(), b"new license");
        assert_eq!(fs::read(root.join("res/b.txt")).unwrap(), b"new resource");
        assert!(!root.join("old-only.dll").exists());
        assert!(!root.join("res/a.txt").exists());
        assert_eq!(
            fs::read(root.join(DATA_DIR_NAME).join("config/settings.json")).unwrap(),
            b"{\"volume\":7}"
        );
        assert!(root.join(MARKER_FILE).is_file());
        let leftovers: Vec<_> = top_level_names(root)
            .unwrap()
            .into_iter()
            .filter(|n| n.to_string_lossy().ends_with(".ow-old"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// Fails every attempt of a forward rename whose destination has the given name; restoring a
    /// `.ow-old` always works.
    struct FailInto(&'static str);

    impl Fs for FailInto {
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            let restoring = from.extension().is_some_and(|ext| ext == "ow-old");
            if !restoring && to.file_name().is_some_and(|name| name == self.0) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected failure",
                ));
            }
            RealFs.rename(from, to)
        }

        fn remove(&self, path: &Path) -> io::Result<()> {
            RealFs.remove(path)
        }
    }

    #[test]
    fn portable_update_failure_after_half_the_entries_restores_the_old_folder_byte_for_byte() {
        let dir = folder();
        let root = dir.path();
        let before = snapshot(root);
        let plan = plan_for(root);
        // All four backups and the first install (LICENSE) succeed; installing the exe fails.
        let error =
            apply_swap(&FailInto("OpenWave Studio.exe"), &plan, FAST, deadline()).unwrap_err();
        assert!(error.restore_problems.is_empty(), "{error}");
        assert_eq!(snapshot(root), before);
    }

    #[test]
    fn portable_update_failure_while_backing_up_restores_the_old_folder() {
        let dir = folder();
        let root = dir.path();
        let before = snapshot(root);
        let plan = plan_for(root);
        let error =
            apply_swap(&FailInto("old-only.dll.ow-old"), &plan, FAST, deadline()).unwrap_err();
        assert!(error.restore_problems.is_empty(), "{error}");
        assert_eq!(snapshot(root), before);
    }

    #[test]
    fn portable_update_staged_marker_and_data_entries_are_skipped() {
        let dir = folder();
        let root = dir.path();
        let staging = root.join(STAGING_DIR);
        write(&staging.join(MARKER_FILE), b"malicious marker");
        write(
            &staging.join(DATA_DIR_NAME).join("config/settings.json"),
            b"overwritten",
        );
        let plan = plan_for(root);
        assert!(
            !plan
                .installs
                .iter()
                .any(|s| is_protected(s.to.file_name().unwrap()))
        );
        apply_swap(&RealFs, &plan, FAST, deadline()).unwrap();
        assert_eq!(fs::read(root.join(MARKER_FILE)).unwrap(), b"");
        assert_eq!(
            fs::read(root.join(DATA_DIR_NAME).join("config/settings.json")).unwrap(),
            b"{\"volume\":7}"
        );
        assert_eq!(
            fs::read(root.join("OpenWave Studio.exe")).unwrap(),
            b"new exe"
        );
    }

    /// Fails the first `failures` attempts of the rename into `name`, then succeeds.
    struct Flaky {
        name: &'static str,
        failures: u32,
        seen: Cell<u32>,
        attempts: RefCell<Vec<Instant>>,
    }

    impl Fs for Flaky {
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            if from.starts_with(to.parent().unwrap().join(STAGING_DIR))
                && to.file_name().is_some_and(|name| name == self.name)
            {
                self.attempts.borrow_mut().push(Instant::now());
                self.seen.set(self.seen.get() + 1);
                if self.seen.get() <= self.failures {
                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, "locked"));
                }
            }
            RealFs.rename(from, to)
        }

        fn remove(&self, path: &Path) -> io::Result<()> {
            RealFs.remove(path)
        }
    }

    #[test]
    fn portable_update_rename_failing_twice_then_succeeding_completes_the_swap() {
        let dir = folder();
        let root = dir.path();
        let plan = plan_for(root);
        let fs = Flaky {
            name: "OpenWave Studio.exe",
            failures: 2,
            seen: Cell::new(0),
            attempts: RefCell::new(Vec::new()),
        };
        apply_swap(&fs, &plan, FAST, deadline()).unwrap();
        assert_eq!(fs.seen.get(), 3);
        assert_eq!(
            fs::read(root.join("OpenWave Studio.exe")).unwrap(),
            b"new exe"
        );
        assert_eq!(fs::read(root.join("LICENSE")).unwrap(), b"new license");
        // The wait grows between attempts.
        let attempts = fs.attempts.borrow();
        assert!(attempts[1] - attempts[0] >= FAST.first_backoff);
        assert!(attempts[2] - attempts[1] >= FAST.first_backoff * 2);
    }

    #[test]
    fn portable_update_retries_stop_at_the_deadline() {
        let started = Instant::now();
        let result: io::Result<()> = retry(
            Instant::now() + Duration::from_millis(40),
            Duration::from_millis(5),
            || Err(io::Error::new(io::ErrorKind::PermissionDenied, "always")),
        );
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn portable_update_an_empty_package_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("OpenWave Studio.exe"), b"old");
        let staging = dir.path().join(STAGING_DIR);
        fs::create_dir_all(&staging).unwrap();
        write(&staging.join(MARKER_FILE), b"");
        assert!(plan_swap(dir.path(), &staging).is_err());
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn portable_update_stage_extracts_the_package() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = zip_of(&[
            ("OpenWave Studio.exe", b"new"),
            ("LICENSE", b"l"),
            ("portable", b""),
        ]);
        let staging = stage(dir.path(), &bytes).unwrap();
        assert_eq!(staging, dir.path().join(STAGING_DIR));
        assert_eq!(
            fs::read(staging.join("OpenWave Studio.exe")).unwrap(),
            b"new"
        );
    }

    #[test]
    fn portable_update_stage_refuses_paths_that_escape_and_packages_without_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let escape = zip_of(&[("OpenWave Studio.exe", b"x"), ("../evil.txt", b"x")]);
        assert!(stage(dir.path(), &escape).is_err());
        assert!(!dir.path().join("evil.txt").exists());
        assert!(!dir.path().join(STAGING_DIR).exists());
        let no_exe = zip_of(&[("LICENSE", b"x")]);
        assert!(stage(dir.path(), &no_exe).is_err());
        assert!(stage(dir.path(), b"not a zip").is_err());
    }

    #[test]
    fn portable_update_helper_arguments_round_trip() {
        let args = HelperArgs {
            pid: 4242,
            dir: PathBuf::from("C:/Music Apps/OpenWave"),
            relaunch: true,
        };
        assert_eq!(HelperArgs::parse(args.to_args()), Some(Ok(args.clone())));
        let quit = HelperArgs {
            relaunch: false,
            ..args
        };
        assert_eq!(HelperArgs::parse(quit.to_args()), Some(Ok(quit)));
    }

    #[test]
    fn portable_update_other_command_lines_are_not_helper_runs() {
        assert_eq!(HelperArgs::parse([OsString::from("--version")]), None);
        assert_eq!(HelperArgs::parse(Vec::<OsString>::new()), None);
        let bad = HelperArgs::parse([OsString::from(APPLY_FLAG)]);
        assert!(matches!(bad, Some(Err(_))));
    }

    #[test]
    fn portable_update_feed_targets_cover_both_windows_architectures() {
        assert_eq!(feed_target("x86_64"), Some("windows-x86_64-portable"));
        assert_eq!(feed_target("aarch64"), Some("windows-aarch64-portable"));
        assert_eq!(feed_target("riscv64"), None);
    }

    #[test]
    fn portable_update_only_transient_errors_are_retried() {
        let calls = Cell::new(0);
        let result: io::Result<()> = retry(
            Instant::now() + Duration::from_secs(5),
            Duration::from_millis(1),
            || {
                calls.set(calls.get() + 1);
                Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
            },
        );
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
        assert!(is_transient(&io::Error::from_raw_os_error(32)));
        assert!(is_transient(&io::Error::new(
            io::ErrorKind::PermissionDenied,
            "x"
        )));
        assert!(!is_transient(&io::Error::new(
            io::ErrorKind::InvalidData,
            "x"
        )));
    }

    #[test]
    fn portable_update_protection_ignores_trailing_dots_and_spaces() {
        assert!(is_protected(OsStr::new("portable.")));
        assert!(is_protected(OsStr::new("Portable . ")));
        assert!(is_protected(OsStr::new("OpenWave Studio Data ")));
        assert!(!is_protected(OsStr::new("portable.txt")));
    }

    #[test]
    fn portable_update_a_users_bak_file_is_treated_like_any_other_entry() {
        let dir = folder();
        let root = dir.path();
        write(&root.join("notes.bak"), b"user's own backup");
        write(
            &root.join("old.ow-old"),
            b"leftover of an interrupted update",
        );
        let plan = plan_for(root);
        assert!(plan.backups.iter().any(|s| s.from.ends_with("notes.bak")));
        assert!(!plan.backups.iter().any(|s| s.from.ends_with("old.ow-old")));
    }

    #[test]
    fn portable_update_the_program_is_swapped_last_and_adjacent() {
        let dir = folder();
        let plan = plan_for(dir.path());
        let ops = plan.ordered();
        let last_two = &ops[ops.len() - 2..];
        assert!(matches!(last_two[0], Op::Backup(s) if s.from.ends_with("OpenWave Studio.exe")));
        assert!(matches!(last_two[1], Op::Install(s) if s.to.ends_with("OpenWave Studio.exe")));
        assert_eq!(ops.len(), plan.backups.len() + plan.installs.len());
    }

    /// Everything works until the program has been moved aside; then every operation fails, as
    /// if the process had been killed at that point.
    struct DiesAfterProgramBackup {
        dead: Cell<bool>,
    }

    impl Fs for DiesAfterProgramBackup {
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            if self.dead.get() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "process killed"));
            }
            RealFs.rename(from, to)?;
            if to.ends_with("OpenWave Studio.exe.ow-old") {
                self.dead.set(true);
            }
            Ok(())
        }

        fn remove(&self, path: &Path) -> io::Result<()> {
            if self.dead.get() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "process killed"));
            }
            RealFs.remove(path)
        }
    }

    #[test]
    fn portable_update_aborting_right_after_the_program_backup_leaves_a_documented_state() {
        let dir = folder();
        let root = dir.path();
        let plan = plan_for(root);
        let fs = DiesAfterProgramBackup {
            dead: Cell::new(false),
        };
        assert!(apply_swap(&fs, &plan, FAST, deadline()).is_err());
        // The program is the only thing missing, its old copy is intact beside the folder's
        // contents, the new one is still staged, and user data and marker are untouched.
        assert!(!root.join("OpenWave Studio.exe").exists());
        assert_eq!(
            fs::read(root.join("OpenWave Studio.exe.ow-old")).unwrap(),
            b"old exe"
        );
        assert_eq!(
            fs::read(root.join(STAGING_DIR).join("OpenWave Studio.exe")).unwrap(),
            b"new exe"
        );
        assert_eq!(fs::read(root.join("LICENSE")).unwrap(), b"new license");
        assert!(root.join(MARKER_FILE).is_file());
        assert_eq!(
            fs::read(root.join(DATA_DIR_NAME).join("config/settings.json")).unwrap(),
            b"{\"volume\":7}"
        );
    }

    #[test]
    fn portable_update_a_failed_update_is_reported_once_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join(DATA_DIR_NAME).join("logs");
        assert_eq!(take_failure(&logs), None);
        record_failure(dir.path(), "injected failure (restore incomplete: none)");
        let message = take_failure(&logs).expect("a failure was recorded");
        assert!(message.contains("injected failure"), "{message}");
        assert!(message.contains(LOG_FILE), "{message}");
        assert!(message.contains("old version was kept"), "{message}");
        assert_eq!(take_failure(&logs), None);
    }

    #[test]
    fn portable_update_a_failed_launch_puts_the_update_back() {
        let mut slot: Option<&str> = None;
        let error = launch_or_restore(&mut slot, "update", || {
            Err(io::Error::new(io::ErrorKind::NotFound, "no helper"))
        });
        assert!(error.is_err());
        assert_eq!(slot, Some("update"));
        let mut slot: Option<&str> = None;
        assert!(launch_or_restore(&mut slot, "update", || Ok(())).is_ok());
        assert_eq!(slot, None);
    }
}
