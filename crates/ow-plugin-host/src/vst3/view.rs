//! VST3 editor views: creating the plugin's `IPlugView`, attaching it to an OpenWave window and
//! honouring its resize requests through `IPlugFrame`.
//!
//! Main thread only. Everything the window and the view share lives in an [`EditorState`] behind
//! `RefCell`s that are only borrowed for a single call, never across a call into the plugin, so a
//! plugin that calls back into the frame from inside `attached` or `onSize` cannot trip a
//! double borrow.

#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper};

use crate::editor::{EditorWindow, WindowEvent};

/// The platform's `IPlugView` type name.
fn platform_type() -> FIDString {
    if cfg!(windows) {
        kPlatformTypeHWND
    } else {
        kPlatformTypeNSView
    }
}

/// One plugin's editor: its window, its view and the frame object the view calls back into.
#[derive(Default)]
pub(super) struct EditorState {
    window: RefCell<Option<EditorWindow>>,
    view: RefCell<Option<ComPtr<IPlugView>>>,
    frame: RefCell<Option<ComWrapper<PlugFrame>>>,
    resizable: Cell<bool>,
    /// True while this code resizes the window for the view or the view for the window.
    resizing: Cell<bool>,
    /// A size the view asked for before the window could take it (during `attached`).
    pending: Cell<Option<(u32, u32)>>,
}

impl EditorState {
    /// The window's client size.
    pub(super) fn size(&self) -> Option<(u32, u32)> {
        self.window.borrow().as_ref().map(EditorWindow::client_size)
    }
}

/// The `IPlugFrame` the view reports its size changes to.
pub(super) struct PlugFrame {
    state: Weak<EditorState>,
}

impl Class for PlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(&self, _view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        let Some(state) = self.state.upgrade() else {
            return kResultFalse;
        };
        if new_size.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: non-null rectangle from the plugin.
        let rect = unsafe { *new_size };
        let width = u32::try_from(rect.right - rect.left).unwrap_or(0);
        let height = u32::try_from(rect.bottom - rect.top).unwrap_or(0);
        if width == 0 || height == 0 {
            return kInvalidArgument;
        }
        if state.resizing.get() {
            // The plugin answered our own `onSize` with a request: the window is already right.
            return kResultOk;
        }
        let applied = match state.window.try_borrow() {
            Ok(window) => window.as_ref().map(|window| {
                state.resizing.set(true);
                window.set_client_size(width, height);
                state.resizing.set(false);
            }),
            Err(_) => None,
        };
        if applied.is_none() {
            // Asked while the window is being set up: applied when `attached` returns.
            state.pending.set(Some((width, height)));
            return kResultOk;
        }
        // The host tells the view the size it got (the SDK's contract for `resizeView`).
        notify_size(&state, width, height);
        kResultOk
    }
}

/// Calls `onSize` with `width` by `height`.
fn notify_size(state: &EditorState, width: u32, height: u32) {
    let view = state.view.borrow().clone();
    let Some(view) = view else {
        return;
    };
    let mut rect = ViewRect {
        left: 0,
        top: 0,
        right: i32::try_from(width).unwrap_or(i32::MAX),
        bottom: i32::try_from(height).unwrap_or(i32::MAX),
    };
    state.resizing.set(true);
    // SAFETY: a live view and a valid rectangle.
    unsafe { view.onSize(&mut rect) };
    state.resizing.set(false);
}

/// Opens the editor of `controller` in a new window titled `title`. Brings the window forward
/// when the editor is already open.
pub(super) fn open(
    state: &Rc<EditorState>,
    controller: &ComPtr<IEditController>,
    title: &str,
) -> Result<(), String> {
    if let Some(window) = state.window.borrow().as_ref() {
        window.show();
        return Ok(());
    }
    // SAFETY: a live controller; the call returns an owned view or null.
    let view = unsafe { ComPtr::from_raw(controller.createView(c"editor".as_ptr())) }
        .ok_or_else(|| "the plugin has no editor".to_string())?;
    // SAFETY: a live view for the calls below.
    unsafe {
        if view.isPlatformTypeSupported(platform_type()) != kResultTrue {
            return Err("the plugin's editor cannot be shown on this platform".into());
        }
    }
    let mut rect = ViewRect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: as above.
    let known = unsafe { view.getSize(&mut rect) } == kResultOk;
    let (mut width, mut height) = if known {
        (
            u32::try_from(rect.right - rect.left).unwrap_or(0),
            u32::try_from(rect.bottom - rect.top).unwrap_or(0),
        )
    } else {
        (0, 0)
    };
    if width == 0 || height == 0 {
        (width, height) = (480, 320);
    }
    // SAFETY: as above.
    let resizable = unsafe { view.canResize() } == kResultTrue;
    let window = EditorWindow::create(title, width, height, resizable)?;
    let handle = window.native_handle();
    *state.window.borrow_mut() = Some(window);
    *state.view.borrow_mut() = Some(view.clone());
    state.resizable.set(resizable);
    state.pending.set(None);

    let frame = ComWrapper::new(PlugFrame {
        state: Rc::downgrade(state),
    });
    let frame_ptr = frame
        .to_com_ptr::<IPlugFrame>()
        .ok_or_else(|| "the frame object is not an IPlugFrame".to_string())?;
    *state.frame.borrow_mut() = Some(frame);
    // SAFETY: a live view, frame and native parent handle.
    let attached = unsafe {
        view.setFrame(frame_ptr.as_ptr());
        view.attached(handle, platform_type())
    };
    if attached != kResultOk {
        close(state);
        return Err("the plugin's editor could not attach to its window".into());
    }
    // The size may have changed while attaching.
    // SAFETY: as above.
    if unsafe { view.getSize(&mut rect) } == kResultOk {
        let attached_width = u32::try_from(rect.right - rect.left).unwrap_or(0);
        let attached_height = u32::try_from(rect.bottom - rect.top).unwrap_or(0);
        if attached_width > 0
            && attached_height > 0
            && (attached_width, attached_height) != (width, height)
            && let Some(window) = state.window.borrow().as_ref()
        {
            state.resizing.set(true);
            window.set_client_size(attached_width, attached_height);
            state.resizing.set(false);
        }
    }
    if let Some((pending_width, pending_height)) = state.pending.take() {
        if let Some(window) = state.window.borrow().as_ref() {
            state.resizing.set(true);
            window.set_client_size(pending_width, pending_height);
            state.resizing.set(false);
        }
        notify_size(state, pending_width, pending_height);
    }
    if let Some(window) = state.window.borrow().as_ref() {
        window.show();
    }
    Ok(())
}

/// Removes the view from its window and destroys the window. Idempotent.
pub(super) fn close(state: &EditorState) {
    let view = state.view.borrow_mut().take();
    if let Some(view) = view {
        // SAFETY: a live view: detached first, then released with the frame cleared.
        unsafe {
            view.removed();
            view.setFrame(std::ptr::null_mut());
        }
    }
    state.frame.borrow_mut().take();
    state.window.borrow_mut().take();
    state.pending.set(None);
}

/// Handles what the user did to the window. Returns false once the editor has been closed.
pub(super) fn poll(state: &EditorState) -> bool {
    let events = match state.window.borrow().as_ref() {
        Some(window) => window.poll(),
        None => return false,
    };
    for event in events {
        match event {
            WindowEvent::CloseRequested => {
                close(state);
                return false;
            }
            WindowEvent::Resized { width, height } if state.resizable.get() => {
                user_resized(state, width, height);
            }
            WindowEvent::Resized { .. } => {}
        }
    }
    true
}

/// The user resized the window: the view gets the nearest size it accepts.
fn user_resized(state: &EditorState, width: u32, height: u32) {
    let view = state.view.borrow().clone();
    let Some(view) = view else {
        return;
    };
    let mut rect = ViewRect {
        left: 0,
        top: 0,
        right: i32::try_from(width).unwrap_or(i32::MAX),
        bottom: i32::try_from(height).unwrap_or(i32::MAX),
    };
    // SAFETY: a live view and a valid rectangle.
    unsafe { view.checkSizeConstraint(&mut rect) };
    let accepted = (
        u32::try_from(rect.right - rect.left).unwrap_or(width),
        u32::try_from(rect.bottom - rect.top).unwrap_or(height),
    );
    if accepted != (width, height)
        && let Some(window) = state.window.borrow().as_ref()
    {
        state.resizing.set(true);
        window.set_client_size(accepted.0, accepted.1);
        state.resizing.set(false);
    }
    notify_size(state, accepted.0, accepted.1);
}
