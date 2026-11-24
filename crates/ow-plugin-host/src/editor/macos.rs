//! Editor windows on macOS: one `NSWindow` per editor, created on the main thread.
//!
//! No window delegate: the owner polls ([`Window::poll`]) from its main-thread `service` pass,
//! which notices that the user closed the window (it is no longer visible although it was not
//! minimised) or resized it (its content size changed without this code asking for it).

#![allow(unsafe_code, unused_unsafe)]

use std::cell::Cell;
use std::ffi::c_void;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSBackingStoreType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::WindowEvent;

pub(super) struct Window {
    window: Retained<NSWindow>,
    shown: Cell<bool>,
    /// The content size last seen or set, in points.
    last: Cell<(u32, u32)>,
}

fn style(resizable: bool) -> NSWindowStyleMask {
    let base =
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
    if resizable {
        base | NSWindowStyleMask::Resizable
    } else {
        base
    }
}

impl Window {
    pub(super) fn create(
        title: &str,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| "editor windows must be created on the main thread".to_string())?;
        // Windows need the application object to exist (the app has one; a test host may not).
        let _ = NSApplication::sharedApplication(mtm);
        let rect = NSRect::new(
            NSPoint::new(200.0, 200.0),
            NSSize::new(f64::from(width), f64::from(height)),
        );
        // SAFETY: a fresh allocation on the main thread, initialised once.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc::<NSWindow>(),
                rect,
                style(resizable),
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: the window is owned by the `Retained` above, not by AppKit's close handling.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        window.center();
        Ok(Self {
            window,
            shown: Cell::new(false),
            last: Cell::new((width, height)),
        })
    }

    pub(super) fn native_handle(&self) -> *mut c_void {
        self.window
            .contentView()
            .map_or(std::ptr::null_mut(), |view| {
                Retained::as_ptr(&view) as *mut c_void
            })
    }

    pub(super) fn client_size(&self) -> (u32, u32) {
        self.window.contentView().map_or((0, 0), |view| {
            let size = view.frame().size;
            (size.width.max(0.0) as u32, size.height.max(0.0) as u32)
        })
    }

    pub(super) fn set_client_size(&self, width: u32, height: u32) {
        self.last.set((width, height));
        self.window
            .setContentSize(NSSize::new(f64::from(width), f64::from(height)));
    }

    pub(super) fn set_resizable(&self, resizable: bool) {
        self.window.setStyleMask(style(resizable));
    }

    pub(super) fn set_title(&self, title: &str) {
        self.window.setTitle(&NSString::from_str(title));
    }

    pub(super) fn show(&self) {
        self.window.makeKeyAndOrderFront(None);
        self.shown.set(true);
    }

    pub(super) fn poll(&self) -> Vec<WindowEvent> {
        let mut events = Vec::new();
        if self.shown.get() && !self.window.isVisible() && !self.window.isMiniaturized() {
            self.shown.set(false);
            events.push(WindowEvent::CloseRequested);
            return events;
        }
        let size = self.client_size();
        if size != self.last.get() && size.0 > 0 && size.1 > 0 {
            self.last.set(size);
            events.push(WindowEvent::Resized {
                width: size.0,
                height: size.1,
            });
        }
        events
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.window.orderOut(None);
        self.window.close();
    }
}

pub(super) fn pump_messages() {}
