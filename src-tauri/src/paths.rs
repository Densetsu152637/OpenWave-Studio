//! Portable mode and data locations.
//!
//! Tauri has no portable mode, so this module decides where settings, caches, logs, the
//! WebView2 user-data folder and the default projects folder live. With a file named
//! `portable` beside the executable (Windows), beside the AppImage (Linux) or beside the
//! `.app` bundle (macOS; not inside it, which would break code signing), everything lives under
//! `<that folder>/OpenWave Studio Data/`. Otherwise the platform directories supplied by
//! Tauri's path resolver are used.

use std::io;
use std::path::{Path, PathBuf};

/// Name of the marker file that switches on portable mode.
pub const MARKER_FILE: &str = "portable";
/// Folder (beside the marker) that holds all portable data.
pub const DATA_DIR_NAME: &str = "OpenWave Studio Data";

/// Platform directories used when portable mode is off (from Tauri's path resolver).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultDirs {
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
    pub logs: PathBuf,
    pub documents: PathBuf,
}

/// Resolved data locations for this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub portable: bool,
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub plugin_cache_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub projects_dir: PathBuf,
    /// WebView2 (and WebKitGTK) user-data folder; `None` means the platform default.
    pub webview_data_dir: Option<PathBuf>,
}

impl Paths {
    /// Resolves locations for the running executable.
    pub fn detect(defaults: DefaultDirs) -> Self {
        let exe = std::env::current_exe().ok();
        let appimage = appimage_path();
        let base = exe
            .as_deref()
            .and_then(|exe| marker_base_dir(exe, appimage.as_deref(), cfg!(target_os = "macos")));
        Self::resolve(base.as_deref(), defaults)
    }

    /// Resolves locations given the folder where the `portable` marker would sit.
    pub fn resolve(marker_base: Option<&Path>, defaults: DefaultDirs) -> Self {
        match marker_base.filter(|base| base.join(MARKER_FILE).is_file()) {
            Some(base) => Self::portable_in(&base.join(DATA_DIR_NAME)),
            None => Self {
                portable: false,
                plugin_cache_dir: defaults.cache.join("plugins"),
                projects_dir: defaults.documents.join("OpenWave Studio"),
                data_dir: defaults.data,
                config_dir: defaults.config,
                cache_dir: defaults.cache,
                logs_dir: defaults.logs,
                webview_data_dir: None,
            },
        }
    }

    fn portable_in(root: &Path) -> Self {
        Self {
            portable: true,
            data_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            cache_dir: root.join("cache"),
            plugin_cache_dir: root.join("cache").join("plugins"),
            logs_dir: root.join("logs"),
            projects_dir: root.join("Projects"),
            webview_data_dir: Some(root.join("webview")),
        }
    }

    /// Creates every directory this layout names.
    pub fn ensure_dirs(&self) -> io::Result<()> {
        let dirs = [
            Some(&self.data_dir),
            Some(&self.config_dir),
            Some(&self.cache_dir),
            Some(&self.plugin_cache_dir),
            Some(&self.logs_dir),
            Some(&self.projects_dir),
            self.webview_data_dir.as_ref(),
        ];
        dirs.into_iter()
            .flatten()
            .try_for_each(std::fs::create_dir_all)
    }
}

#[cfg(target_os = "linux")]
fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(not(target_os = "linux"))]
fn appimage_path() -> Option<PathBuf> {
    None
}

/// The folder in which the `portable` marker is looked up: the AppImage's folder, the folder
/// holding the `.app` bundle (`macos_bundle`), or else the executable's folder.
pub fn marker_base_dir(exe: &Path, appimage: Option<&Path>, macos_bundle: bool) -> Option<PathBuf> {
    if let Some(image) = appimage {
        return image.parent().map(Path::to_path_buf);
    }
    if macos_bundle {
        let bundle = exe
            .ancestors()
            .find(|dir| dir.extension().is_some_and(|ext| ext == "app"));
        if let Some(bundle) = bundle {
            return bundle.parent().map(Path::to_path_buf);
        }
    }
    exe.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> DefaultDirs {
        DefaultDirs {
            data: PathBuf::from("/platform/data"),
            config: PathBuf::from("/platform/config"),
            cache: PathBuf::from("/platform/cache"),
            logs: PathBuf::from("/platform/logs"),
            documents: PathBuf::from("/platform/documents"),
        }
    }

    #[test]
    fn marker_present_selects_portable_layout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MARKER_FILE), b"").unwrap();
        let paths = Paths::resolve(Some(dir.path()), defaults());
        let root = dir.path().join(DATA_DIR_NAME);
        assert!(paths.portable);
        assert_eq!(paths.data_dir, root);
        assert_eq!(paths.config_dir, root.join("config"));
        assert_eq!(paths.logs_dir, root.join("logs"));
        assert_eq!(paths.projects_dir, root.join("Projects"));
        assert_eq!(paths.webview_data_dir, Some(root.join("webview")));
        assert!(paths.plugin_cache_dir.starts_with(&root));
        assert!(paths.cache_dir.starts_with(&root));
    }

    #[test]
    fn marker_absent_uses_platform_directories() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::resolve(Some(dir.path()), defaults());
        assert!(!paths.portable);
        assert_eq!(paths.data_dir, PathBuf::from("/platform/data"));
        assert_eq!(paths.config_dir, PathBuf::from("/platform/config"));
        assert_eq!(paths.logs_dir, PathBuf::from("/platform/logs"));
        assert_eq!(
            paths.projects_dir,
            PathBuf::from("/platform/documents/OpenWave Studio")
        );
        assert_eq!(paths.webview_data_dir, None);
        assert!(!dir.path().join(DATA_DIR_NAME).exists());
    }

    #[test]
    fn unknown_marker_base_is_not_portable() {
        assert!(!Paths::resolve(None, defaults()).portable);
    }

    #[test]
    fn a_directory_named_portable_is_not_a_marker() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(MARKER_FILE)).unwrap();
        assert!(!Paths::resolve(Some(dir.path()), defaults()).portable);
    }

    #[test]
    fn ensure_dirs_creates_the_portable_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MARKER_FILE), b"").unwrap();
        let paths = Paths::resolve(Some(dir.path()), defaults());
        paths.ensure_dirs().unwrap();
        assert!(paths.config_dir.is_dir());
        assert!(paths.plugin_cache_dir.is_dir());
        assert!(paths.projects_dir.is_dir());
        assert!(paths.webview_data_dir.as_ref().unwrap().is_dir());
    }

    #[test]
    fn marker_base_is_beside_the_executable_by_default() {
        let exe = Path::new("/apps/openwave/openwave-studio");
        assert_eq!(
            marker_base_dir(exe, None, false),
            Some(PathBuf::from("/apps/openwave"))
        );
    }

    #[test]
    fn marker_base_is_beside_the_appimage() {
        let exe = Path::new("/tmp/.mount_abc/usr/bin/openwave-studio");
        let image = Path::new("/home/me/OpenWave.AppImage");
        assert_eq!(
            marker_base_dir(exe, Some(image), false),
            Some(PathBuf::from("/home/me"))
        );
    }

    #[test]
    fn marker_base_is_beside_the_app_bundle_on_macos() {
        let exe = Path::new("/Users/me/Music/OpenWave Studio.app/Contents/MacOS/openwave-studio");
        assert_eq!(
            marker_base_dir(exe, None, true),
            Some(PathBuf::from("/Users/me/Music"))
        );
        // Without a bundle (for example `cargo run`) it falls back to the executable's folder.
        let dev = Path::new("/work/target/debug/openwave-studio");
        assert_eq!(
            marker_base_dir(dev, None, true),
            Some(PathBuf::from("/work/target/debug"))
        );
    }
}
