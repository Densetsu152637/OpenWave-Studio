//! No editor windows on this platform (Linux: a non-goal of the editor track).

use std::ffi::c_void;

use super::WindowEvent;

/// Never constructed: [`Window::create`] always fails.
pub(super) struct Window {
    never: std::convert::Infallible,
}

impl Window {
    pub(super) fn create(
        _title: &str,
        _width: u32,
        _height: u32,
        _resizable: bool,
    ) -> Result<Self, String> {
        Err("plugin editor windows are not available on this platform".into())
    }

    pub(super) fn native_handle(&self) -> *mut c_void {
        match self.never {}
    }

    pub(super) fn client_size(&self) -> (u32, u32) {
        match self.never {}
    }

    pub(super) fn set_client_size(&self, _width: u32, _height: u32) {
        match self.never {}
    }

    pub(super) fn set_resizable(&self, _resizable: bool) {
        match self.never {}
    }

    pub(super) fn set_title(&self, _title: &str) {
        match self.never {}
    }

    pub(super) fn show(&self) {
        match self.never {}
    }

    pub(super) fn poll(&self) -> Vec<WindowEvent> {
        match self.never {}
    }
}

pub(super) fn pump_messages() {}
