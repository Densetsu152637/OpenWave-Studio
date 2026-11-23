//! Editor windows on Windows: one top-level Win32 window per editor, created with
//! `CreateWindowExW` on the plugin main thread.
//!
//! The window procedure only records what the user did (close, resize) in the window's state;
//! [`Window::poll`] hands it out. A close request does not destroy the window: the owner closes
//! the plugin's editor first and then drops the window.

#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::Once;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CS_DBLCLKS, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, GWL_STYLE, GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, HMENU, IDC_ARROW,
    LoadCursorW, MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SIZE_MINIMIZED, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, TranslateMessage, WM_CLOSE, WM_SIZE, WNDCLASSW, WS_CAPTION,
    WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_THICKFRAME,
};

use super::WindowEvent;

const CLASS_NAME: &[u16] = &[
    b'O' as u16,
    b'p' as u16,
    b'e' as u16,
    b'n' as u16,
    b'W' as u16,
    b'a' as u16,
    b'v' as u16,
    b'e' as u16,
    b'E' as u16,
    b'd' as u16,
    b'i' as u16,
    b't' as u16,
    b'o' as u16,
    b'r' as u16,
    0,
];

/// `COLOR_WINDOW + 1`, the way `WNDCLASS::hbrBackground` spells a system colour.
const WINDOW_BRUSH: usize = 6;

static REGISTER: Once = Once::new();

/// What the window procedure records.
#[derive(Default)]
struct State {
    events: RefCell<Vec<WindowEvent>>,
    /// Non-zero while this code changes the window itself: size messages are not the user's.
    quiet: Cell<u32>,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the pointer was stored by `Window::create` and is cleared before the state drops.
    let state = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State).as_ref() };
    if let Some(state) = state {
        match message {
            WM_CLOSE => {
                state.events.borrow_mut().push(WindowEvent::CloseRequested);
                return 0;
            }
            WM_SIZE if state.quiet.get() == 0 && wparam as u32 != SIZE_MINIMIZED => {
                let width = (lparam & 0xFFFF) as u32;
                let height = ((lparam >> 16) & 0xFFFF) as u32;
                if width > 0 && height > 0 {
                    let mut events = state.events.borrow_mut();
                    // Dragging produces a stream of sizes: only the latest matters.
                    if let Some(WindowEvent::Resized { .. }) = events.last() {
                        events.pop();
                    }
                    events.push(WindowEvent::Resized { width, height });
                }
            }
            _ => {}
        }
    }
    // SAFETY: forwarding the message this procedure was called with.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn register_class() -> Result<(), String> {
    let mut failure = None;
    REGISTER.call_once(|| {
        // SAFETY: plain Win32 calls with valid arguments.
        unsafe {
            let class = WNDCLASSW {
                style: CS_DBLCLKS,
                lpfnWndProc: Some(window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: GetModuleHandleW(null()),
                hIcon: null_mut(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                hbrBackground: WINDOW_BRUSH as *mut c_void,
                lpszMenuName: null(),
                lpszClassName: CLASS_NAME.as_ptr(),
            };
            if RegisterClassW(&class) == 0 {
                failure = Some("could not register the editor window class".to_string());
            }
        }
    });
    match failure {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

fn style(resizable: bool) -> u32 {
    let base = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    if resizable {
        base | WS_THICKFRAME | WS_MAXIMIZEBOX
    } else {
        base
    }
}

pub(super) struct Window {
    hwnd: HWND,
    state: Box<State>,
}

impl Window {
    pub(super) fn create(
        title: &str,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<Self, String> {
        register_class()?;
        let state = Box::<State>::default();
        let (outer_width, outer_height) = outer_size(width, height, style(resizable));
        let title = wide(title);
        // SAFETY: the class is registered; every pointer outlives the call.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                CLASS_NAME.as_ptr(),
                title.as_ptr(),
                style(resizable),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                outer_width,
                outer_height,
                null_mut(),
                null_mut() as HMENU,
                GetModuleHandleW(null()),
                null(),
            )
        };
        if hwnd.is_null() {
            return Err("could not create the editor window".into());
        }
        // SAFETY: the state lives as long as the window (`Drop` clears the pointer first).
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*state as *const State as isize);
        }
        Ok(Self { hwnd, state })
    }

    pub(super) fn native_handle(&self) -> *mut c_void {
        self.hwnd
    }

    pub(super) fn client_size(&self) -> (u32, u32) {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: a live window and a valid out pointer.
        unsafe { GetClientRect(self.hwnd, &mut rect) };
        (
            u32::try_from(rect.right - rect.left).unwrap_or(0),
            u32::try_from(rect.bottom - rect.top).unwrap_or(0),
        )
    }

    pub(super) fn set_client_size(&self, width: u32, height: u32) {
        // SAFETY: a live window.
        let current = unsafe { GetWindowLongPtrW(self.hwnd, GWL_STYLE) } as u32;
        let (outer_width, outer_height) = outer_size(width, height, current);
        self.state.quiet.set(self.state.quiet.get() + 1);
        // SAFETY: a live window; the size message it sends is not recorded (`quiet`).
        unsafe {
            SetWindowPos(
                self.hwnd,
                null_mut(),
                0,
                0,
                outer_width,
                outer_height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.state.quiet.set(self.state.quiet.get() - 1);
    }

    pub(super) fn set_resizable(&self, resizable: bool) {
        let (width, height) = self.client_size();
        // SAFETY: a live window.
        unsafe { SetWindowLongPtrW(self.hwnd, GWL_STYLE, style(resizable) as isize) };
        self.set_client_size(width, height);
    }

    pub(super) fn set_title(&self, title: &str) {
        let title = wide(title);
        // SAFETY: a live window and a NUL-terminated string.
        unsafe { SetWindowTextW(self.hwnd, title.as_ptr()) };
    }

    pub(super) fn show(&self) {
        // SAFETY: a live window.
        unsafe {
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
        }
    }

    pub(super) fn poll(&self) -> Vec<WindowEvent> {
        std::mem::take(&mut *self.state.events.borrow_mut())
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: a live window; messages sent while it is destroyed find no state.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            DestroyWindow(self.hwnd);
        }
    }
}

/// The window size (frame included) for a client area of `width` by `height`.
fn outer_size(width: u32, height: u32, style: u32) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: i32::try_from(width).unwrap_or(i32::MAX),
        bottom: i32::try_from(height).unwrap_or(i32::MAX),
    };
    // SAFETY: a valid rectangle.
    unsafe { AdjustWindowRectEx(&mut rect, style, 0, 0) };
    (rect.right - rect.left, rect.bottom - rect.top)
}

pub(super) fn pump_messages() {
    // SAFETY: a plain message loop on the calling thread.
    unsafe {
        let mut message: MSG = std::mem::zeroed();
        while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}
