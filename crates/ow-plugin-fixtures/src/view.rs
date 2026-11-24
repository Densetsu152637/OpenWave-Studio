//! The fixture editors' native view: a plain child window (Win32) or subview (AppKit) that fills
//! its parent's client area with a solid colour and draws nothing else.
//!
//! The CLAP and the VST3 fixture share it, so what the host-side tests check (a view attaches to
//! the host's window, follows its size and goes away when removed) is the same for both. Linux
//! has no editor windows, and [`ChildView::create`] answers `None` there.

#![allow(unsafe_code)]

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};
    use std::sync::Once;

    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::CreateSolidBrush;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, RegisterClassW,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos, WNDCLASSW, WS_CHILD, WS_VISIBLE,
    };

    const CLASS: &[u16] = &[
        b'O' as u16,
        b'W' as u16,
        b'F' as u16,
        b'i' as u16,
        b'x' as u16,
        b't' as u16,
        b'u' as u16,
        b'r' as u16,
        b'e' as u16,
        b'V' as u16,
        b'i' as u16,
        b'e' as u16,
        b'w' as u16,
        0,
    ];
    static REGISTER: Once = Once::new();

    pub struct ChildView {
        hwnd: HWND,
    }

    impl ChildView {
        pub fn create(parent: *mut c_void, width: u32, height: u32) -> Option<Self> {
            REGISTER.call_once(|| {
                // SAFETY: plain Win32 calls with valid arguments.
                unsafe {
                    let class = WNDCLASSW {
                        style: 0,
                        lpfnWndProc: Some(DefWindowProcW),
                        cbClsExtra: 0,
                        cbWndExtra: 0,
                        hInstance: GetModuleHandleW(null()),
                        hIcon: null_mut(),
                        hCursor: null_mut(),
                        // A dull blue-grey: "drawing nothing special".
                        hbrBackground: CreateSolidBrush(0x00AA_8866),
                        lpszMenuName: null(),
                        lpszClassName: CLASS.as_ptr(),
                    };
                    RegisterClassW(&class);
                }
            });
            // SAFETY: `parent` is the host's window; the class is registered.
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    CLASS.as_ptr(),
                    null(),
                    WS_CHILD | WS_VISIBLE,
                    0,
                    0,
                    i32::try_from(width).unwrap_or(1),
                    i32::try_from(height).unwrap_or(1),
                    parent,
                    null_mut(),
                    GetModuleHandleW(null()),
                    null(),
                )
            };
            (!hwnd.is_null()).then_some(Self { hwnd })
        }

        pub fn set_size(&self, width: u32, height: u32) {
            // SAFETY: a live window.
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    null_mut(),
                    0,
                    0,
                    i32::try_from(width).unwrap_or(1),
                    i32::try_from(height).unwrap_or(1),
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }

        pub fn size(&self) -> (u32, u32) {
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

        /// The parent window the view is a child of (what the host passed).
        pub fn handle(&self) -> *mut c_void {
            self.hwnd
        }
    }

    impl Drop for ChildView {
        fn drop(&mut self) {
            // SAFETY: a live window (the host destroys the parent only after the view is gone).
            unsafe { DestroyWindow(self.hwnd) };
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;

    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::NSView;
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    pub struct ChildView {
        view: Retained<NSView>,
    }

    impl ChildView {
        pub fn create(parent: *mut c_void, width: u32, height: u32) -> Option<Self> {
            let mtm = MainThreadMarker::new()?;
            if parent.is_null() {
                return None;
            }
            let rect = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(f64::from(width), f64::from(height)),
            );
            // SAFETY: `parent` is the host's `NSView`, alive for the call.
            let parent = unsafe { &*(parent as *const NSView) };
            let view = NSView::initWithFrame(mtm.alloc::<NSView>(), rect);
            parent.addSubview(&view);
            Some(Self { view })
        }

        pub fn set_size(&self, width: u32, height: u32) {
            self.view
                .setFrameSize(NSSize::new(f64::from(width), f64::from(height)));
        }

        pub fn size(&self) -> (u32, u32) {
            let size = self.view.frame().size;
            (size.width.max(0.0) as u32, size.height.max(0.0) as u32)
        }

        pub fn handle(&self) -> *mut c_void {
            Retained::as_ptr(&self.view) as *mut c_void
        }
    }

    impl Drop for ChildView {
        fn drop(&mut self) {
            self.view.removeFromSuperview();
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use std::ffi::c_void;

    pub struct ChildView;

    impl ChildView {
        pub fn create(_parent: *mut c_void, _width: u32, _height: u32) -> Option<Self> {
            None
        }

        pub fn set_size(&self, _width: u32, _height: u32) {}

        pub fn size(&self) -> (u32, u32) {
            (0, 0)
        }

        pub fn handle(&self) -> *mut c_void {
            std::ptr::null_mut()
        }
    }
}

pub use platform::ChildView;

/// The view's size the fixtures start with.
pub const DEFAULT_SIZE: (u32, u32) = (320, 200);
