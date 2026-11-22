//! macOS audio workgroups for the DSP workers (architecture section 4).
//!
//! On Apple Silicon the scheduler treats threads in the output device's audio workgroup like the
//! device's IO thread (same deadline, performance cores). [`AudioWorkgroup::default_output`]
//! reads `kAudioDevicePropertyIOThreadOSWorkgroup` from the default output device and every DSP
//! worker joins it with `os_workgroup_join` when the core is built with
//! [`CoreBuilder::audio_workgroup`](crate::CoreBuilder::audio_workgroup). `audio_thread_priority`
//! does not expose workgroups, so this is a small FFI shim over CoreAudio and libSystem.
//!
//! On other platforms every call returns [`WorkgroupError::Unsupported`].

// FFI to CoreAudio and os_workgroup on macOS.
#![allow(unsafe_code)]

use std::fmt;
use std::marker::PhantomData;

/// Why a workgroup could not be obtained or joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkgroupError {
    /// Audio workgroups exist only on macOS.
    Unsupported,
    /// The system reports no default output device.
    NoDevice,
    /// A CoreAudio property query failed with this `OSStatus`.
    Query(i32),
    /// The device returned no workgroup.
    NoWorkgroup,
    /// `os_workgroup_join` failed with this error number.
    Join(i32),
}

impl fmt::Display for WorkgroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("audio workgroups are only available on macOS"),
            Self::NoDevice => f.write_str("there is no default output device"),
            Self::Query(status) => write!(f, "CoreAudio property query failed (OSStatus {status})"),
            Self::NoWorkgroup => f.write_str("the output device has no IO thread workgroup"),
            Self::Join(code) => write!(f, "os_workgroup_join failed (error {code})"),
        }
    }
}

impl std::error::Error for WorkgroupError {}

/// An audio device's IO thread workgroup (`os_workgroup_t`, retained).
pub struct AudioWorkgroup {
    #[cfg(target_os = "macos")]
    raw: std::ptr::NonNull<std::ffi::c_void>,
    #[cfg(not(target_os = "macos"))]
    _unsupported: (),
}

// SAFETY: `os_workgroup_t` objects are reference-counted and thread-safe; joining from any
// thread is the intended use.
unsafe impl Send for AudioWorkgroup {}
// SAFETY: as above; `&AudioWorkgroup` only allows `join`, which is thread-safe.
unsafe impl Sync for AudioWorkgroup {}

/// The calling thread's membership in a workgroup; leaves the workgroup when dropped. Not
/// `Send`: a thread must leave a workgroup on the thread that joined it.
pub struct WorkgroupMembership<'a> {
    #[cfg(target_os = "macos")]
    workgroup: &'a AudioWorkgroup,
    #[cfg(target_os = "macos")]
    token: Box<ffi::JoinToken>,
    _thread_bound: PhantomData<(&'a AudioWorkgroup, *const ())>,
}

#[cfg(target_os = "macos")]
mod ffi {
    use std::ffi::{c_int, c_void};

    #[repr(C)]
    pub(super) struct PropertyAddress {
        pub(super) selector: u32,
        pub(super) scope: u32,
        pub(super) element: u32,
    }

    /// `os_workgroup_join_token_s` is 40 bytes on 64-bit targets; reserve more, aligned.
    #[repr(C, align(8))]
    pub(super) struct JoinToken(pub(super) [u8; 64]);

    const fn four_cc(code: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*code)
    }

    pub(super) const SYSTEM_OBJECT: u32 = 1;
    pub(super) const UNKNOWN_OBJECT: u32 = 0;
    pub(super) const DEFAULT_OUTPUT_DEVICE: u32 = four_cc(b"dOut");
    pub(super) const IO_THREAD_OS_WORKGROUP: u32 = four_cc(b"oswg");
    pub(super) const SCOPE_GLOBAL: u32 = four_cc(b"glob");
    pub(super) const ELEMENT_MAIN: u32 = 0;

    #[allow(non_snake_case)]
    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn AudioObjectGetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            data_size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    // os_workgroup (macOS 11+) and os_object live in libSystem.
    unsafe extern "C" {
        pub(super) fn os_workgroup_join(workgroup: *mut c_void, token: *mut JoinToken) -> c_int;
        pub(super) fn os_workgroup_leave(workgroup: *mut c_void, token: *mut JoinToken);
        pub(super) fn os_release(object: *mut c_void);
    }
}

#[cfg(target_os = "macos")]
impl AudioWorkgroup {
    /// The IO thread workgroup of the current default output device.
    pub fn default_output() -> Result<Self, WorkgroupError> {
        use std::ffi::c_void;
        use std::ptr;

        let mut address = ffi::PropertyAddress {
            selector: ffi::DEFAULT_OUTPUT_DEVICE,
            scope: ffi::SCOPE_GLOBAL,
            element: ffi::ELEMENT_MAIN,
        };
        let mut device: u32 = ffi::UNKNOWN_OBJECT;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: `device` is a writable `AudioObjectID` and `size` says so.
        let status = unsafe {
            ffi::AudioObjectGetPropertyData(
                ffi::SYSTEM_OBJECT,
                &address,
                0,
                ptr::null(),
                &mut size,
                (&mut device as *mut u32).cast(),
            )
        };
        if status != 0 {
            return Err(WorkgroupError::Query(status));
        }
        if device == ffi::UNKNOWN_OBJECT {
            return Err(WorkgroupError::NoDevice);
        }
        address.selector = ffi::IO_THREAD_OS_WORKGROUP;
        let mut workgroup: *mut c_void = ptr::null_mut();
        let mut size = size_of::<*mut c_void>() as u32;
        // SAFETY: `workgroup` is a writable `os_workgroup_t` slot and `size` says so. CoreAudio
        // returns a retained object that we release in `drop`.
        let status = unsafe {
            ffi::AudioObjectGetPropertyData(
                device,
                &address,
                0,
                ptr::null(),
                &mut size,
                (&mut workgroup as *mut *mut c_void).cast(),
            )
        };
        if status != 0 {
            return Err(WorkgroupError::Query(status));
        }
        std::ptr::NonNull::new(workgroup)
            .map(|raw| Self { raw })
            .ok_or(WorkgroupError::NoWorkgroup)
    }

    /// Joins the calling thread to the workgroup until the returned membership is dropped.
    pub fn join(&self) -> Result<WorkgroupMembership<'_>, WorkgroupError> {
        let mut token = Box::new(ffi::JoinToken([0; 64]));
        // SAFETY: `raw` is a live retained workgroup; `token` is caller-allocated storage that
        // outlives the membership and is passed back to `os_workgroup_leave`.
        let code = unsafe { ffi::os_workgroup_join(self.raw.as_ptr(), &mut *token) };
        if code != 0 {
            return Err(WorkgroupError::Join(code));
        }
        Ok(WorkgroupMembership {
            workgroup: self,
            token,
            _thread_bound: PhantomData,
        })
    }
}

#[cfg(target_os = "macos")]
impl Drop for AudioWorkgroup {
    fn drop(&mut self) {
        // SAFETY: we own one retain of the workgroup (see `default_output`).
        unsafe { ffi::os_release(self.raw.as_ptr()) };
    }
}

#[cfg(target_os = "macos")]
impl Drop for WorkgroupMembership<'_> {
    fn drop(&mut self) {
        // SAFETY: same thread (the type is not `Send`), same workgroup and token as the join.
        unsafe { ffi::os_workgroup_leave(self.workgroup.raw.as_ptr(), &mut *self.token) };
    }
}

#[cfg(not(target_os = "macos"))]
impl AudioWorkgroup {
    /// Always [`WorkgroupError::Unsupported`] outside macOS.
    pub fn default_output() -> Result<Self, WorkgroupError> {
        Err(WorkgroupError::Unsupported)
    }

    /// Always [`WorkgroupError::Unsupported`] outside macOS.
    pub fn join(&self) -> Result<WorkgroupMembership<'_>, WorkgroupError> {
        Err(WorkgroupError::Unsupported)
    }
}

impl fmt::Debug for AudioWorkgroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AudioWorkgroup")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plan stage 6 acceptance: on macOS, join the default output device's workgroup from a
    /// real-time thread, or skip with a logged reason when the runner exposes no device.
    #[test]
    fn workgroup_of_the_default_output_can_be_joined() {
        match AudioWorkgroup::default_output() {
            Ok(workgroup) => {
                std::thread::scope(|scope| {
                    scope
                        .spawn(|| {
                            let _priority =
                                audio_thread_priority::promote_current_thread_to_real_time(
                                    128, 48_000,
                                );
                            let membership = workgroup.join();
                            assert!(membership.is_ok(), "join failed: {:?}", membership.err());
                            eprintln!("workgroup: joined the default output device's workgroup");
                        })
                        .join()
                        .expect("join thread");
                });
            }
            Err(error) if cfg!(target_os = "macos") => {
                assert_ne!(error, WorkgroupError::Unsupported);
                eprintln!("workgroup: skipped, no workgroup to join: {error}");
            }
            Err(error) => assert_eq!(error, WorkgroupError::Unsupported),
        }
    }
}
