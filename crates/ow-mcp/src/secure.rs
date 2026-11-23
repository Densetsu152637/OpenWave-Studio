//! Access control for the endpoint directory, the token file and (on Windows) the named pipe:
//! readable and writable by the current user only.
//!
//! * Unix: the directory is mode 0700, the files and the socket 0600.
//! * Windows: a protected DACL (no inherited entries) that grants full access to the current
//!   user's SID and nobody else, applied to the directory, to the files in it and to the pipe.
//!
//! The Win32 calls live here, with `unsafe` allowed for this module only.

use std::io;
use std::path::Path;

/// Creates `path` (and its parents) and makes it private to the current user.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    restrict_dir(path)
}

#[cfg(unix)]
pub fn restrict_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(unix)]
pub fn restrict_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// True when a process with this id is running (or exists but belongs to someone else).
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists; it sends nothing.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
pub use windows::{PipeSecurity, process_alive, restrict_dir, restrict_file};

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
        PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// `STILL_ACTIVE`: the exit code of a process that has not exited.
    const STILL_ACTIVE: u32 = 259;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The current user's SID in string form (`S-1-5-21-...`).
    fn current_user_sid() -> io::Result<String> {
        // SAFETY: every pointer passed is to a local that outlives the call; the token handle
        // is closed on every path; the SID string `ConvertSidToStringSidW` allocates is copied
        // and freed with `LocalFree`.
        unsafe {
            let mut token: HANDLE = ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let result = (|| {
                let mut length = 0_u32;
                // The first call only reports the size the answer needs.
                GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut length);
                if length == 0 {
                    return Err(io::Error::last_os_error());
                }
                // 8-byte aligned storage for the TOKEN_USER structure.
                let mut buffer = vec![0_u64; (length as usize).div_ceil(8)];
                if GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast::<c_void>(),
                    length,
                    &mut length,
                ) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
                let mut text: *mut u16 = ptr::null_mut();
                if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut len = 0;
                while *text.add(len) != 0 {
                    len += 1;
                }
                let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
                LocalFree(text.cast::<c_void>());
                Ok(sid)
            })();
            CloseHandle(token);
            result
        }
    }

    /// A security descriptor built from an SDDL string, freed on drop.
    struct Descriptor(*mut c_void);

    impl Descriptor {
        fn from_sddl(sddl: &str) -> io::Result<Self> {
            let text = wide(sddl);
            let mut descriptor: *mut c_void = ptr::null_mut();
            // SAFETY: `text` is NUL-terminated and outlives the call; the output is a local.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(descriptor))
        }
    }

    impl Drop for Descriptor {
        fn drop(&mut self) {
            // SAFETY: allocated by `ConvertStringSecurityDescriptorToSecurityDescriptorW`,
            // which documents `LocalFree`.
            unsafe {
                LocalFree(self.0);
            }
        }
    }

    /// Security attributes for creating the named pipe: current user only.
    pub struct PipeSecurity {
        // Keeps the descriptor alive for as long as the attributes point to it.
        _descriptor: Descriptor,
        attributes: Box<SECURITY_ATTRIBUTES>,
    }

    // SAFETY: the descriptor and the attributes are never mutated after construction.
    unsafe impl Send for PipeSecurity {}
    // SAFETY: as above; only a pointer to immutable data is handed out.
    unsafe impl Sync for PipeSecurity {}

    impl PipeSecurity {
        /// A DACL that grants the current user, and nobody else, full access.
        pub fn current_user() -> io::Result<Self> {
            let sid = current_user_sid()?;
            let descriptor = Descriptor::from_sddl(&format!("D:P(A;;GA;;;{sid})"))?;
            let attributes = Box::new(SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: 0,
            });
            Ok(Self {
                _descriptor: descriptor,
                attributes,
            })
        }

        /// The pointer `CreateNamedPipeW` takes as `lpSecurityAttributes`; valid while `self`
        /// lives.
        pub fn as_ptr(&self) -> *mut c_void {
            ptr::from_ref::<SECURITY_ATTRIBUTES>(&self.attributes)
                .cast_mut()
                .cast::<c_void>()
        }
    }

    /// Replaces the DACL of `path` with a protected one granting the current user full access
    /// (inherited by files and folders created inside when `inherit`).
    fn restrict(path: &Path, inherit: bool) -> io::Result<()> {
        let sid = current_user_sid()?;
        let flags = if inherit { "OICI" } else { "" };
        let descriptor = Descriptor::from_sddl(&format!("D:P(A;{flags};FA;;;{sid})"))?;
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = ptr::null_mut();
        // SAFETY: the descriptor is valid; the outputs are locals. The returned ACL points into
        // the descriptor, which lives until the end of this function.
        let ok = unsafe {
            GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)
        };
        if ok == 0 || present == 0 {
            return Err(io::Error::last_os_error());
        }
        let name: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `name` is NUL-terminated; `dacl` is valid (above); owner, group and SACL are
        // not set.
        let status = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    }

    /// Makes the directory private to the current user (its files inherit that).
    pub fn restrict_dir(path: &Path) -> io::Result<()> {
        restrict(path, true)
    }

    /// Makes the file private to the current user.
    pub fn restrict_file(path: &Path) -> io::Result<()> {
        restrict(path, false)
    }

    /// True when a process with this id is running.
    pub fn process_alive(pid: u32) -> bool {
        // SAFETY: plain query calls; the handle is closed.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0_u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE
        }
    }
}
