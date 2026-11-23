//! Loading a VST3 module: the shared library of a `.vst3` file or bundle and its plugin factory.
//!
//! The layout follows the VST3 module format: on Windows a `.vst3` is a DLL (or a bundle folder
//! with the DLL in `Contents/<arch>-win`), on macOS a bundle with the binary in
//! `Contents/MacOS`, on Linux a bundle with the library in `Contents/<arch>-linux`. The module
//! entry point (`InitDll`, `BundleEntry`, `ModuleEntry`) runs once per load and the exit point
//! never does: plugin libraries stay loaded for the life of the process, like CLAP's.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use vst3::Steinberg::*;
use vst3::{ComPtr, Interface};

use super::host::read_cstr;

/// `PClassInfo::category` of the classes that are audio plugins.
pub(super) const AUDIO_CATEGORY: &str = "Audio Module Class";

/// A loaded module.
pub(super) struct Module {
    pub(super) factory: ComPtr<IPluginFactory>,
    /// Kept so the library is never unloaded (and the bundle reference stays valid).
    _library: libloading::Library,
    #[cfg(target_os = "macos")]
    _bundle: objc2_core_foundation::CFRetained<objc2_core_foundation::CFBundle>,
}

/// One class of a module's factory.
#[derive(Debug, Clone)]
pub(super) struct ClassInfo {
    pub(super) cid: [u8; 16],
    pub(super) category: String,
    pub(super) name: String,
    pub(super) vendor: String,
    pub(super) version: String,
    /// `|`-separated sub categories (`Fx|Dynamics`, `Instrument|Synth`).
    pub(super) sub_categories: String,
}

thread_local! {
    /// Loaded modules by path (plugin main thread only; modules are never unloaded).
    static MODULES: RefCell<HashMap<PathBuf, Rc<Module>>> = RefCell::new(HashMap::new());
}

/// The four words of a class id, in the order the SDK writes them (`FUID::toString`): the same
/// whatever the platform's `TUID` byte layout is (Windows lays the first three words out like a
/// COM `GUID`, other platforms big-endian).
fn words(cid: &[u8; 16]) -> [u32; 4] {
    let big = |at: usize| u32::from_be_bytes([cid[at], cid[at + 1], cid[at + 2], cid[at + 3]]);
    if cfg!(windows) {
        [
            u32::from_le_bytes([cid[0], cid[1], cid[2], cid[3]]),
            u32::from_be_bytes([cid[5], cid[4], cid[7], cid[6]]),
            big(8),
            big(12),
        ]
    } else {
        [big(0), big(4), big(8), big(12)]
    }
}

/// The class id as 32 uppercase hexadecimal digits (four 8-digit words), which is what
/// `vst3:<id>` kinds hold. The text is the same on every platform, so a project made on one
/// opens on another.
pub(super) fn cid_text(cid: &[u8; 16]) -> String {
    words(cid)
        .iter()
        .map(|word| format!("{word:08X}"))
        .collect()
}

/// The class id behind [`cid_text`].
pub(super) fn parse_cid(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 || !text.is_ascii() {
        return None;
    }
    let mut words = [0_u32; 4];
    for (index, word) in words.iter_mut().enumerate() {
        *word = u32::from_str_radix(&text[index * 8..index * 8 + 8], 16).ok()?;
    }
    Some(tuid_bytes(&vst3::uid(
        words[0], words[1], words[2], words[3],
    )))
}

/// The shared library inside a `.vst3` file or bundle.
fn library_file(path: &Path) -> Result<PathBuf, String> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    crate::scan::executable_of(path)
        .ok_or_else(|| format!("{} has no VST3 module for this platform", path.display()))
}

/// Loads (or reuses) the module at `path`. Main thread only.
pub(super) fn load(path: &Path) -> Result<Rc<Module>, String> {
    if let Some(module) = MODULES.with(|modules| modules.borrow().get(path).cloned()) {
        return Ok(module);
    }
    let module = Rc::new(open(path)?);
    MODULES.with(|modules| {
        modules
            .borrow_mut()
            .insert(path.to_path_buf(), Rc::clone(&module));
    });
    Ok(module)
}

fn open(path: &Path) -> Result<Module, String> {
    let file = library_file(path)?;
    let describe =
        |error: &dyn std::fmt::Display| format!("could not load {}: {error}", path.display());

    // SAFETY: loading a plugin library runs the plugin's initialisation code, which is the whole
    // point of hosting it. Scanning loads candidates in a separate process first, so a library
    // that crashes while loading is found there.
    #[cfg(unix)]
    let (library, handle) = {
        let library = unsafe { libloading::os::unix::Library::new(&file) }
            .map_err(|error| describe(&error))?;
        let handle = library.into_raw();
        // SAFETY: the handle was just taken from a live library.
        let library = unsafe { libloading::os::unix::Library::from_raw(handle) };
        (libloading::Library::from(library), handle)
    };
    #[cfg(not(unix))]
    let library = unsafe { libloading::Library::new(&file) }.map_err(|error| describe(&error))?;

    #[cfg(target_os = "macos")]
    let bundle = {
        use objc2_core_foundation::{CFBundle, CFURL};
        let url = CFURL::from_directory_path(path)
            .ok_or_else(|| format!("{} is not a bundle folder", path.display()))?;
        let bundle = CFBundle::new(None, Some(&url))
            .ok_or_else(|| format!("{} is not a loadable bundle", path.display()))?;
        // SAFETY: `BundleEntry` is the VST3 module entry point on macOS.
        if let Ok(entry) = unsafe {
            library.get::<unsafe extern "system" fn(*mut c_void) -> bool>(b"BundleEntry\0")
        } {
            let reference = &*bundle as *const CFBundle as *mut c_void;
            if !unsafe { entry(reference) } {
                return Err(describe(&"the module entry point refused to start"));
            }
        }
        bundle
    };
    #[cfg(windows)]
    {
        // SAFETY: `InitDll` is the optional VST3 module entry point on Windows.
        if let Ok(entry) =
            unsafe { library.get::<unsafe extern "system" fn() -> bool>(b"InitDll\0") }
            && !unsafe { entry() }
        {
            return Err(describe(&"the module entry point refused to start"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // SAFETY: `ModuleEntry` is the VST3 module entry point on Linux.
        if let Ok(entry) = unsafe {
            library.get::<unsafe extern "system" fn(*mut c_void) -> bool>(b"ModuleEntry\0")
        } && !unsafe { entry(handle) }
        {
            return Err(describe(&"the module entry point refused to start"));
        }
    }
    #[cfg(target_os = "macos")]
    let _ = handle;

    // SAFETY: `GetPluginFactory` is the VST3 factory entry point.
    let factory = unsafe {
        let get = library
            .get::<unsafe extern "system" fn() -> *mut IPluginFactory>(b"GetPluginFactory\0")
            .map_err(|_| describe(&"the library has no VST3 plugin factory"))?;
        ComPtr::from_raw(get()).ok_or_else(|| describe(&"the plugin factory is null"))?
    };
    Ok(Module {
        factory,
        _library: library,
        #[cfg(target_os = "macos")]
        _bundle: bundle,
    })
}

impl Module {
    /// Passes the host context to a factory that wants it (`IPluginFactory3`).
    pub(super) fn set_host_context(&self, context: *mut FUnknown) {
        if let Some(factory) = self.factory.cast::<IPluginFactory3>() {
            // SAFETY: a live factory and a live context.
            unsafe { factory.setHostContext(context) };
        }
    }

    /// Every class the factory offers (any category).
    pub(super) fn classes(&self) -> Vec<ClassInfo> {
        // SAFETY: a live factory; the info structs are plain data.
        unsafe {
            let mut vendor = String::new();
            let mut factory_info: PFactoryInfo = std::mem::zeroed();
            if self.factory.getFactoryInfo(&mut factory_info) == kResultOk {
                vendor = read_cstr(&factory_info.vendor);
            }
            let factory2 = self.factory.cast::<IPluginFactory2>();
            let count = self.factory.countClasses().max(0);
            let mut classes = Vec::new();
            for index in 0..count {
                let mut info2: PClassInfo2 = std::mem::zeroed();
                if let Some(factory2) = &factory2
                    && factory2.getClassInfo2(index, &mut info2) == kResultOk
                {
                    classes.push(ClassInfo {
                        cid: tuid_bytes(&info2.cid),
                        category: read_cstr(&info2.category),
                        name: read_cstr(&info2.name),
                        vendor: match read_cstr(&info2.vendor) {
                            own if own.is_empty() => vendor.clone(),
                            own => own,
                        },
                        version: read_cstr(&info2.version),
                        sub_categories: read_cstr(&info2.subCategories),
                    });
                    continue;
                }
                let mut info: PClassInfo = std::mem::zeroed();
                if self.factory.getClassInfo(index, &mut info) == kResultOk {
                    classes.push(ClassInfo {
                        cid: tuid_bytes(&info.cid),
                        category: read_cstr(&info.category),
                        name: read_cstr(&info.name),
                        vendor: vendor.clone(),
                        version: String::new(),
                        sub_categories: String::new(),
                    });
                }
            }
            classes
        }
    }

    /// Creates an instance of class `cid` as interface `I`.
    pub(super) fn create<I: Interface>(&self, cid: &[u8; 16]) -> Option<ComPtr<I>> {
        let mut object: *mut c_void = std::ptr::null_mut();
        // SAFETY: a live factory; both ids are 16 bytes; the out pointer is valid.
        let result = unsafe {
            self.factory.createInstance(
                cid.as_ptr() as *const c_char,
                I::IID.as_ptr() as *const c_char,
                &mut object,
            )
        };
        if result != kResultOk {
            return None;
        }
        // SAFETY: on success the factory returned an owned reference to an `I`.
        unsafe { ComPtr::from_raw(object as *mut I) }
    }
}

/// A `TUID` as bytes.
pub(super) fn tuid_bytes(tuid: &TUID) -> [u8; 16] {
    std::array::from_fn(|index| tuid[index] as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_ids_round_trip_through_their_text_and_read_like_the_sdk() {
        let cid = tuid_bytes(&vst3::uid(
            0x0011_2233,
            0x4455_6677,
            0x8899_AABB,
            0xCCDD_EEFF,
        ));
        let text = cid_text(&cid);
        assert_eq!(text, "00112233445566778899AABBCCDDEEFF");
        assert_eq!(parse_cid(&text), Some(cid));
        assert_eq!(parse_cid("short"), None);
        assert_eq!(parse_cid(&"G".repeat(32)), None);
    }
}
