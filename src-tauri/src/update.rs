//! Self-update for installed builds.
//!
//! The app follows one of two channels, `main` (stable) and `experimental`. Each channel's feed
//! is the fixed-URL `latest.json` asset of the `updater-<channel>` GitHub Release, written by the
//! publish scripts after the versioned release exists. The updater checks 20 s after start and
//! every 6 h, downloads a newer signed build in the background, and installs it when the app
//! quits or when the user asks to restart now. The signature in the feed is verified against the
//! public key in `tauri.conf.json`.
//!
//! After a channel switch exactly one check accepts any different version (so moving from
//! experimental back to main can "downgrade"); that relaxation is never persisted.
//!
//! A Windows portable folder cannot use the installer-based path: it checks the feed with the
//! custom target `windows-<arch>-portable`, stages the verified zip and lets
//! [`crate::portable_update`] swap the files when the app exits or restarts. Linux AppImages and
//! macOS `.app` bundles use the standard replace-in-place updater.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Notify;

use crate::paths::Paths;
use crate::portable_update;
use crate::translocation;

/// The repository whose releases carry the update feeds.
const REPOSITORY: &str = "Densetsu152637/OpenWave-Studio";
/// Delay before the first check, so it does not compete with opening the window.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(20);
/// Interval between checks while the app stays open.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Network timeout for one feed or installer request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Event carrying every status change to the UI.
const STATUS_EVENT: &str = "update://status";
/// Marks experimental versions.
const EXPERIMENTAL_MARK: &str = "-experimental";
/// Message shown (and logged) for a package-managed Linux install.
pub const PACKAGE_MANAGED_MESSAGE: &str = "Update OpenWave Studio through your package manager";
/// Message shown for a translocated macOS app (see [`crate::translocation`]).
pub const TRANSLOCATED_MESSAGE: &str =
    "Move OpenWave Studio out of its download location (or clear its quarantine) to enable updates";

/// The release line an install follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Main,
    Experimental,
}

impl Channel {
    /// The channel a build was released on: the one it follows until the user switches.
    pub fn of_version(version: &str) -> Self {
        if version.contains(EXPERIMENTAL_MARK) {
            Self::Experimental
        } else {
            Self::Main
        }
    }

    /// Name used in the feed release tag (`updater-<name>`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Experimental => "experimental",
        }
    }

    /// The fixed feed URL of this channel.
    pub fn feed_url(self) -> String {
        format!(
            "https://github.com/{REPOSITORY}/releases/download/updater-{}/latest.json",
            self.name()
        )
    }
}

/// Which remote versions a check accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparator {
    /// Only a version newer than the running one (every ordinary check).
    Newer,
    /// Any version different from the running one (the single check after a channel switch).
    AnyDifferent,
}

impl Comparator {
    pub fn accepts(self, current: &Version, remote: &Version) -> bool {
        match self {
            Self::Newer => remote > current,
            Self::AnyDifferent => remote != current,
        }
    }
}

/// Why this install does not update itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disabled {
    /// Linux without `$APPIMAGE`: a deb or rpm install, updated by the package manager.
    PackageManaged,
    /// A quarantined macOS app running from a read-only translocated copy.
    Translocated,
}

impl Disabled {
    pub fn message(self) -> &'static str {
        match self {
            Self::PackageManaged => PACKAGE_MANAGED_MESSAGE,
            Self::Translocated => TRANSLOCATED_MESSAGE,
        }
    }
}

/// Decides whether updates are disabled for this install. Pure, so tests can cover every
/// platform: `appimage` is the value of `$APPIMAGE`. Windows portable folders update through
/// [`crate::portable_update`], so they are not disabled.
pub fn disabled_reason(
    linux: bool,
    appimage: Option<&OsStr>,
    translocated: bool,
) -> Option<Disabled> {
    if linux && appimage.is_none_or(OsStr::is_empty) {
        return Some(Disabled::PackageManaged);
    }
    if translocated {
        return Some(Disabled::Translocated);
    }
    None
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Downloading {
        version: String,
    },
    Ready {
        version: String,
        notes: Option<String>,
    },
    Failed {
        message: String,
    },
    Disabled {
        reason: Disabled,
        message: String,
    },
}

/// What the UI shows about updates. Mirrored in `src/lib/update/index.ts`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    current_version: String,
    channel: Channel,
    /// Whether the app checks by itself (off in development builds and when updates are disabled).
    automatic: bool,
    #[serde(flatten)]
    state: UpdateState,
}

/// The saved channel choice.
#[derive(Serialize, Deserialize)]
struct Preferences {
    channel: Channel,
}

pub struct Updater {
    inner: Mutex<Inner>,
    check_requested: Notify,
    /// Held for a whole check, so a requested check never overlaps a scheduled one.
    checking: tokio::sync::Mutex<()>,
    disabled: Option<Disabled>,
    preferences: Option<PathBuf>,
    /// The folder of a Windows portable install, which updates through `portable_update`.
    portable_dir: Option<PathBuf>,
}

struct Inner {
    channel: Channel,
    state: UpdateState,
    /// A downloaded update waiting to be installed. A portable update is already extracted to the
    /// staging folder, so its bytes are empty.
    ready: Option<(Update, Vec<u8>)>,
    /// Set by a channel switch; consumed by the next check that reaches the feed.
    relaxed: bool,
}

/// What a finished check does to the stored downloaded update.
enum ReadyChange {
    Keep,
    Clear,
    Set(Box<(Update, Vec<u8>)>),
}

impl Inner {
    /// Records the outcome of a check that ran for `channel`. A check that finishes after the
    /// user switched channel is stale and changes nothing; returns whether it was applied.
    fn settle(&mut self, channel: Channel, state: UpdateState, ready: ReadyChange) -> bool {
        if self.channel != channel {
            return false;
        }
        match ready {
            ReadyChange::Keep => {}
            ReadyChange::Clear => self.ready = None,
            ReadyChange::Set(update) => self.ready = Some(*update),
        }
        self.state = state;
        true
    }
}

impl Updater {
    fn status<R: Runtime>(&self, app: &AppHandle<R>) -> UpdateStatus {
        let inner = self.inner.lock().expect("updater state");
        let state = match self.disabled {
            Some(reason) => UpdateState::Disabled {
                reason,
                message: reason.message().to_owned(),
            },
            None => inner.state.clone(),
        };
        UpdateStatus {
            current_version: app.package_info().version.to_string(),
            channel: inner.channel,
            automatic: self.disabled.is_none() && automatic(),
            state,
        }
    }

    fn set_state<R: Runtime>(&self, app: &AppHandle<R>, state: UpdateState) {
        self.inner.lock().expect("updater state").state = state;
        let _ = app.emit(STATUS_EVENT, self.status(app));
    }
}

/// Development builds only check when asked.
fn automatic() -> bool {
    !cfg!(debug_assertions)
}

/// Registers the updater state and starts the periodic check.
pub fn setup<R: Runtime>(app: &AppHandle<R>) {
    let version = app.package_info().version.to_string();
    let preferences = app
        .try_state::<Paths>()
        .map(|paths| paths.config_dir.join("update.json"));
    let saved = preferences
        .as_ref()
        .and_then(|file| std::fs::read(file).ok())
        .and_then(|bytes| serde_json::from_slice::<Preferences>(&bytes).ok());
    let channel = saved.map_or_else(|| Channel::of_version(&version), |saved| saved.channel);
    let portable = app.try_state::<Paths>().is_some_and(|paths| paths.portable);
    let portable_dir = if cfg!(windows) && portable {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(PathBuf::from))
    } else {
        None
    };
    let disabled = disabled_reason(
        cfg!(target_os = "linux"),
        std::env::var_os("APPIMAGE").as_deref(),
        translocation::is_translocated_exe(),
    );
    // A portable update that failed in the helper leaves a note: show it until the next check.
    let last_failure = app
        .try_state::<Paths>()
        .filter(|paths| paths.portable)
        .and_then(|paths| portable_update::take_failure(&paths.logs_dir));
    let state = last_failure.map_or(UpdateState::Idle, |message| UpdateState::Failed { message });
    app.manage(Updater {
        inner: Mutex::new(Inner {
            channel,
            state,
            ready: None,
            relaxed: false,
        }),
        check_requested: Notify::new(),
        checking: tokio::sync::Mutex::new(()),
        disabled,
        preferences,
        portable_dir,
    });

    if let Some(reason) = disabled {
        eprintln!("[openwave] update: checks disabled ({})", reason.message());
        return;
    }
    eprintln!(
        "[openwave] update: version {version}, channel {}, automatic checks {}",
        channel.name(),
        if automatic() {
            "on"
        } else {
            "off (development build)"
        }
    );

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let updater = app.state::<Updater>();
        if automatic() {
            tokio::time::sleep(FIRST_CHECK_AFTER).await;
        } else {
            updater.check_requested.notified().await;
        }
        loop {
            check(&app).await;
            if automatic() {
                // Either the interval passes or a check is requested.
                let _ = tokio::time::timeout(CHECK_EVERY, updater.check_requested.notified()).await;
            } else {
                updater.check_requested.notified().await;
            }
        }
    });
}

/// Installs a downloaded update as the app exits. On Windows the installer takes over and exits
/// the process; it is told not to start the new version, since the user chose to quit.
pub fn install_on_exit<R: Runtime>(app: &AppHandle<R>) {
    let Some(updater) = app.try_state::<Updater>() else {
        return;
    };
    let ready = updater.inner.lock().expect("updater state").ready.take();
    if let Some((update, bytes)) = ready {
        eprintln!("[openwave] update: installing {} on exit", update.version);
        if let Some(dir) = &updater.portable_dir {
            // The helper waits for this process to exit, then swaps the files; no relaunch.
            let mut slot = updater.inner.lock().expect("updater state");
            if let Err(error) =
                portable_update::launch_or_restore(&mut slot.ready, (update, bytes), || {
                    portable_update::launch_helper(dir, false)
                })
            {
                eprintln!("[openwave] update: starting the portable helper failed: {error}");
            }
            return;
        }
        if let Err(error) = update.restart_after_install(false).install(bytes) {
            eprintln!("[openwave] update: install on exit failed: {error}");
        }
    }
}

async fn check<R: Runtime>(app: &AppHandle<R>) {
    let updater = app.state::<Updater>();
    let _checking = updater.checking.lock().await;
    let (channel, downloaded, relaxed) = {
        let mut inner = updater.inner.lock().expect("updater state");
        let downloaded = inner
            .ready
            .as_ref()
            .map(|(update, _)| update.version.clone());
        (
            inner.channel,
            downloaded,
            std::mem::take(&mut inner.relaxed),
        )
    };
    updater.set_state(app, UpdateState::Checking);
    let comparator = if relaxed {
        Comparator::AnyDifferent
    } else {
        Comparator::Newer
    };
    let portable_dir = updater.portable_dir.clone();
    let (state, ready) = match find(app, channel, comparator, portable_dir.is_some()).await {
        Ok(None) => (UpdateState::UpToDate, ReadyChange::Clear),
        Ok(Some(update)) if downloaded.as_deref() == Some(&update.version) => (
            UpdateState::Ready {
                version: update.version,
                notes: update.body,
            },
            ReadyChange::Keep,
        ),
        Ok(Some(update)) => {
            updater.set_state(
                app,
                UpdateState::Downloading {
                    version: update.version.clone(),
                },
            );
            match update.download(|_, _| {}, || {}).await {
                Ok(bytes) => {
                    // A portable zip is extracted now, so "ready" means the swap can run.
                    let (bytes, staged) = match &portable_dir {
                        Some(dir) => (Vec::new(), portable_update::stage(dir, &bytes).map(drop)),
                        None => (bytes, Ok(())),
                    };
                    match staged {
                        Ok(()) => (
                            UpdateState::Ready {
                                version: update.version.clone(),
                                notes: update.body.clone(),
                            },
                            ReadyChange::Set(Box::new((update, bytes))),
                        ),
                        Err(error) => (
                            UpdateState::Failed {
                                message: format!("Preparing {} failed: {error}", update.version),
                            },
                            ReadyChange::Keep,
                        ),
                    }
                }
                Err(error) => (
                    UpdateState::Failed {
                        message: format!("Downloading {} failed: {error}", update.version),
                    },
                    ReadyChange::Keep,
                ),
            }
        }
        Err(message) => {
            // The relaxed check was not used up: give it to the next attempt.
            if relaxed {
                updater.inner.lock().expect("updater state").relaxed = true;
            }
            (UpdateState::Failed { message }, ReadyChange::Keep)
        }
    };
    eprintln!("[openwave] update: {state:?}");
    let applied = updater
        .inner
        .lock()
        .expect("updater state")
        .settle(channel, state, ready);
    if applied {
        let _ = app.emit(STATUS_EVENT, updater.status(app));
    } else {
        eprintln!("[openwave] update: discarded a result for the previous channel");
    }
}

/// An update on `channel` that `comparator` accepts, if the feed offers one.
async fn find<R: Runtime>(
    app: &AppHandle<R>,
    channel: Channel,
    comparator: Comparator,
    portable: bool,
) -> Result<Option<Update>, String> {
    let url = channel.feed_url();
    eprintln!("[openwave] update: checking {url} ({comparator:?}, portable {portable})");
    let endpoint = tauri::Url::parse(&url).map_err(|error| error.to_string())?;
    let mut builder = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|error| error.to_string())?;
    if portable {
        // The feed's `windows-<arch>-portable` entry points at the signed portable zip.
        let target = portable_update::feed_target(std::env::consts::ARCH)
            .ok_or_else(|| "No portable update is published for this architecture.".to_owned())?;
        builder = builder.target(target);
    }
    builder
        .timeout(REQUEST_TIMEOUT)
        .version_comparator(move |current, remote| comparator.accepts(&current, &remote.version))
        .build()
        .map_err(|error| error.to_string())?
        .check()
        .await
        .map_err(|error| format!("Checking for updates failed: {error}"))
}

#[tauri::command]
pub fn update_status<R: Runtime>(
    app: AppHandle<R>,
    updater: tauri::State<'_, Updater>,
) -> UpdateStatus {
    updater.status(&app)
}

/// Starts a check now; the result arrives as a status event. Does nothing, and makes no request,
/// when updates are disabled for this install.
#[tauri::command]
pub fn update_check(updater: tauri::State<'_, Updater>) {
    if updater.disabled.is_none() {
        updater.check_requested.notify_one();
    }
}

/// Switches channel, saves the choice and checks the new channel once with the relaxed
/// comparator.
#[tauri::command]
pub fn update_set_channel<R: Runtime>(
    app: AppHandle<R>,
    updater: tauri::State<'_, Updater>,
    channel: Channel,
) -> Result<UpdateStatus, String> {
    if let Some(file) = &updater.preferences {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        let bytes =
            serde_json::to_vec(&Preferences { channel }).map_err(|error| error.to_string())?;
        std::fs::write(file, bytes).map_err(|error| error.to_string())?;
    }
    {
        let mut inner = updater.inner.lock().expect("updater state");
        if inner.channel != channel {
            inner.channel = channel;
            inner.ready = None;
            inner.state = UpdateState::Idle;
            inner.relaxed = true;
        }
    }
    update_check(updater.clone());
    Ok(updater.status(&app))
}

/// Installs the downloaded update and starts the new version.
#[tauri::command]
pub fn update_restart<R: Runtime>(
    app: AppHandle<R>,
    updater: tauri::State<'_, Updater>,
) -> Result<(), String> {
    install_and_restart(&app, &updater)
}

/// Checks the channel now and, when it offers a build to install, downloads it, installs it and
/// starts the new version. Returns the status when there is nothing to install.
#[tauri::command]
pub async fn update_install_now<R: Runtime>(app: AppHandle<R>) -> Result<UpdateStatus, String> {
    let updater = app.state::<Updater>();
    if let Some(reason) = updater.disabled {
        return Err(reason.message().to_owned());
    }
    check(&app).await;
    let status = updater.status(&app);
    match &status.state {
        UpdateState::Ready { .. } => install_and_restart(&app, &updater).map(|()| status),
        UpdateState::Failed { message } => Err(message.clone()),
        _ => Ok(status),
    }
}

fn install_and_restart<R: Runtime>(app: &AppHandle<R>, updater: &Updater) -> Result<(), String> {
    let ready = updater.inner.lock().expect("updater state").ready.take();
    let Some((update, bytes)) = ready else {
        return Err("No update has been downloaded.".into());
    };
    if let Some(dir) = &updater.portable_dir {
        // The helper swaps the files once this process has exited, then starts the new version.
        // When it cannot be started the update stays ready, so the user can try again.
        let mut slot = updater.inner.lock().expect("updater state");
        portable_update::launch_or_restore(&mut slot.ready, (update, bytes), || {
            portable_update::launch_helper(dir, true)
        })
        .map_err(|error| format!("Starting the update failed: {error}"))?;
        drop(slot);
        app.exit(0);
        return Ok(());
    }
    // On Windows the installer takes over here, exits this process and starts the new version.
    if let Err(error) = update.install(bytes) {
        // The downloaded build is used up, so the panel must not keep offering it.
        let message = format!("Installing {} failed: {error}", update.version);
        updater.set_state(
            app,
            UpdateState::Failed {
                message: message.clone(),
            },
        );
        return Err(message);
    }
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("version")
    }

    #[test]
    fn a_build_follows_the_channel_it_was_released_on() {
        assert_eq!(Channel::of_version("0.1.0"), Channel::Main);
        assert_eq!(Channel::of_version("0.2.0-rc.1"), Channel::Main);
        assert_eq!(
            Channel::of_version("0.1.9-experimental"),
            Channel::Experimental
        );
    }

    #[test]
    fn update_feed_urls_are_fixed_per_channel() {
        assert_eq!(
            Channel::Main.feed_url(),
            "https://github.com/Densetsu152637/OpenWave-Studio/releases/download/updater-main/latest.json"
        );
        assert_eq!(
            Channel::Experimental.feed_url(),
            "https://github.com/Densetsu152637/OpenWave-Studio/releases/download/updater-experimental/latest.json"
        );
        for channel in [Channel::Main, Channel::Experimental] {
            tauri::Url::parse(&channel.feed_url()).expect("valid url");
        }
    }

    #[test]
    fn update_channel_names_round_trip_through_preferences() {
        let saved = serde_json::to_string(&Preferences {
            channel: Channel::Experimental,
        })
        .expect("serialize");
        assert_eq!(saved, r#"{"channel":"experimental"}"#);
        let loaded: Preferences = serde_json::from_str(&saved).expect("deserialize");
        assert_eq!(loaded.channel, Channel::Experimental);
    }

    #[test]
    fn update_comparator_newer_only_accepts_greater_versions() {
        let newer = Comparator::Newer;
        assert!(newer.accepts(&v("0.1.8-experimental"), &v("0.1.9-experimental")));
        assert!(newer.accepts(&v("0.1.9-experimental"), &v("0.2.0")));
        assert!(!newer.accepts(&v("0.1.9-experimental"), &v("0.1.9-experimental")));
        // Moving from experimental to an older stable is not an update.
        assert!(!newer.accepts(&v("0.1.9-experimental"), &v("0.1.0")));
    }

    #[test]
    fn update_comparator_after_a_switch_accepts_any_different_version() {
        let relaxed = Comparator::AnyDifferent;
        assert!(relaxed.accepts(&v("0.1.9-experimental"), &v("0.1.0")));
        assert!(relaxed.accepts(&v("0.1.0"), &v("0.1.9-experimental")));
        // Already on the channel's current build: up to date.
        assert!(!relaxed.accepts(&v("0.1.0"), &v("0.1.0")));
    }

    #[test]
    fn update_is_disabled_for_package_managed_linux_installs() {
        let appimage = OsStr::new("/home/u/OpenWave.AppImage");
        assert_eq!(
            disabled_reason(true, None, false),
            Some(Disabled::PackageManaged)
        );
        assert_eq!(
            disabled_reason(true, Some(OsStr::new("")), false),
            Some(Disabled::PackageManaged)
        );
        assert_eq!(disabled_reason(true, Some(appimage), false), None);
        // Package management only concerns Linux; other platforms have no `$APPIMAGE`.
        assert_eq!(disabled_reason(false, None, false), None);
    }

    #[test]
    fn update_is_disabled_for_translocated_macos_apps() {
        assert_eq!(
            disabled_reason(false, None, true),
            Some(Disabled::Translocated)
        );
    }

    #[test]
    fn update_portable_windows_folders_are_not_disabled() {
        // Portable state no longer disables checks: Windows portable folders update through
        // `portable_update`, AppImage and `.app` portables through the standard updater.
        assert_eq!(disabled_reason(false, None, false), None);
    }

    fn inner(channel: Channel) -> Inner {
        Inner {
            channel,
            state: UpdateState::Idle,
            ready: None,
            relaxed: false,
        }
    }

    #[test]
    fn update_a_result_for_the_previous_channel_is_discarded() {
        let mut state = inner(Channel::Main);
        let applied = state.settle(
            Channel::Experimental,
            UpdateState::UpToDate,
            ReadyChange::Clear,
        );
        assert!(!applied);
        assert!(matches!(state.state, UpdateState::Idle));
    }

    #[test]
    fn update_a_result_for_the_current_channel_is_applied() {
        let mut state = inner(Channel::Main);
        assert!(state.settle(Channel::Main, UpdateState::UpToDate, ReadyChange::Clear));
        assert!(matches!(state.state, UpdateState::UpToDate));
    }

    #[test]
    fn update_package_managed_message_is_the_documented_text() {
        assert_eq!(
            Disabled::PackageManaged.message(),
            "Update OpenWave Studio through your package manager"
        );
    }
}
