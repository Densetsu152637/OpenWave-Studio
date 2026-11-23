//! Editor windows: plain platform windows OpenWave creates for plugin editors (architecture
//! section 8).
//!
//! A plugin editor is given a native parent view to draw into and nothing else, so the window is
//! ours: a Win32 top-level window on Windows ([`win32`]), an `NSWindow` on macOS ([`macos`]).
//! Linux has no editor windows (a non-goal of the editor track): [`AVAILABLE`] is false there and
//! [`EditorWindow::create`] fails.
//!
//! Everything here runs on the plugin main thread (see [`crate::executor`]); on Windows and
//! macOS that is the thread that runs the app's event loop, which also delivers the window's
//! messages. The window never calls into plugin code: what happened to it (the user closed it or
//! resized it) is recorded and handed out by [`EditorWindow::poll`], which the plugin
//! implementations call from their main-thread `service` pass. That keeps window messages from
//! re-entering a plugin that is in the middle of one of its own calls.

use std::ffi::c_void;

#[cfg(windows)]
mod win32;
#[cfg(windows)]
use win32 as platform;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(not(any(windows, target_os = "macos")))]
mod none;
#[cfg(not(any(windows, target_os = "macos")))]
use none as platform;

/// True when this platform can show plugin editor windows (Windows and macOS).
pub const AVAILABLE: bool = cfg!(any(windows, target_os = "macos"));

/// What happened to an editor window since the last [`EditorWindow::poll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEvent {
    /// The user asked to close the window (the window is still there: the owner closes it).
    CloseRequested,
    /// The user resized the window; the new client size in pixels.
    Resized {
        /// Client width.
        width: u32,
        /// Client height.
        height: u32,
    },
}

/// A top-level window with an empty client area for a plugin's editor view.
///
/// Main thread only (the type is not `Send`). Dropping the window destroys it; the plugin's view
/// must have been removed from it first.
pub struct EditorWindow {
    inner: platform::Window,
}

impl EditorWindow {
    /// Creates a hidden window whose client area is `width` by `height` pixels.
    pub fn create(title: &str, width: u32, height: u32, resizable: bool) -> Result<Self, String> {
        Ok(Self {
            inner: platform::Window::create(title, width.max(1), height.max(1), resizable)?,
        })
    }

    /// The native view plugins attach to: an `HWND` on Windows, an `NSView*` on macOS.
    pub fn native_handle(&self) -> *mut c_void {
        self.inner.native_handle()
    }

    /// The client area size in pixels.
    pub fn client_size(&self) -> (u32, u32) {
        self.inner.client_size()
    }

    /// Resizes the client area. Not reported by [`poll`](Self::poll) (the caller asked for it).
    pub fn set_client_size(&self, width: u32, height: u32) {
        self.inner.set_client_size(width.max(1), height.max(1));
    }

    /// Lets the user resize the window (or not).
    pub fn set_resizable(&self, resizable: bool) {
        self.inner.set_resizable(resizable);
    }

    /// Changes the title bar text.
    pub fn set_title(&self, title: &str) {
        self.inner.set_title(title);
    }

    /// Shows the window and brings it to the front.
    pub fn show(&self) {
        self.inner.show();
    }

    /// What the user did to the window since the last call.
    pub fn poll(&self) -> Vec<WindowEvent> {
        self.inner.poll()
    }

    /// Runs the thread's pending window messages. The app's event loop does this itself; tests
    /// and the fixture host call it where nothing else does.
    pub fn pump_messages() {
        platform::pump_messages();
    }
}
