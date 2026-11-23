//! The host side of VST3: the COM objects OpenWave hands to plugins.
//!
//! The `vst3` crate gives bindings and a way to implement interfaces from Rust, nothing more, so
//! the host interfaces are ours (architecture section 8):
//!
//! * [`HostApp`] (`IHostApplication`): the context passed to `initialize`; it also makes the
//!   [`Message`] and [`AttributeList`] objects plugins use to talk between their component and
//!   controller.
//! * [`ComponentHandler`] (`IComponentHandler`, `IComponentHandler2`): the controller's way to
//!   tell the host about edits made in the plugin's editor and about restart requests.
//! * [`MemStream`] (`IBStream`): state is exchanged through memory streams.
//! * [`ParamChanges`] and [`EventList`] (`IParameterChanges`, `IEventList`): the per-block
//!   parameter and note events. They are preallocated and only use interior mutability, so
//!   filling them on the audio thread allocates nothing.

#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper, Interface};

use crate::hosted::{EditPhase, EditorEdit};

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A parameter value on its way to the audio side.
#[derive(Debug, Clone, Copy)]
pub(super) struct Edit {
    pub id: u32,
    /// Normalised, as the processor wants it.
    pub value: f64,
}

/// What the plugin asked of the host.
#[derive(Default)]
pub(super) struct Flags {
    /// The plugin wants to be re-activated (its buses or latency changed).
    pub restart: AtomicBool,
    pub latency: AtomicBool,
    pub params: AtomicBool,
    pub state_dirty: AtomicBool,
}

impl Flags {
    pub(super) fn any(&self) -> bool {
        [
            &self.restart,
            &self.latency,
            &self.params,
            &self.state_dirty,
        ]
        .iter()
        .any(|flag| flag.load(Ordering::Acquire))
    }
}

/// Where editor-side parameter edits go (the cache hands the plugin its device id).
pub(super) type Sink = Arc<dyn Fn(EditorEdit) + Send + Sync>;

/// State the control side and the host objects share.
pub(super) struct Shared {
    pub flags: Flags,
    /// Normalised edits for the processor. Pushed by `set_params` and by the component handler.
    pub edits: Mutex<rtrb::Producer<Edit>>,
    /// The plain values this host last sent (or the editor last reported), by parameter id.
    pub applied: Mutex<HashMap<u32, f64>>,
    pub sink: Mutex<Option<Sink>>,
    /// The controller, for plain/normalised conversion in the handler.
    pub controller: Mutex<Option<ComPtr<IEditController>>>,
}

impl Shared {
    pub(super) fn new(edits: rtrb::Producer<Edit>) -> Arc<Self> {
        Arc::new(Self {
            flags: Flags::default(),
            edits: Mutex::new(edits),
            applied: Mutex::new(HashMap::new()),
            sink: Mutex::new(None),
            controller: Mutex::new(None),
        })
    }

    pub(super) fn controller(&self) -> Option<ComPtr<IEditController>> {
        lock(&self.controller).clone()
    }

    /// Plain value of normalised `value` of parameter `id`.
    pub(super) fn plain(&self, id: u32, value: f64) -> f64 {
        match self.controller() {
            // SAFETY: the controller is a live COM object; the call is a pure mapping.
            Some(controller) => unsafe { controller.normalizedParamToPlain(id, value) },
            None => value,
        }
    }

    pub(super) fn normalized(&self, id: u32, plain: f64) -> f64 {
        match self.controller() {
            // SAFETY: as above.
            Some(controller) => unsafe { controller.plainParamToNormalized(id, plain) },
            None => plain,
        }
        .clamp(0.0, 1.0)
    }

    fn report(&self, edit: EditorEdit) {
        let sink = lock(&self.sink).clone();
        if let Some(sink) = sink {
            sink(edit);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Strings

/// Copies `text` into a NUL-terminated UTF-16 buffer.
pub(super) fn write_wide(text: &str, out: &mut [u16]) {
    let mut length = 0;
    for (unit, slot) in text.encode_utf16().zip(out.iter_mut()) {
        *slot = unit;
        length += 1;
    }
    if length < out.len() {
        out[length] = 0;
    } else if let Some(last) = out.last_mut() {
        *last = 0;
    }
}

/// Reads a NUL-terminated UTF-16 buffer.
pub(super) fn read_wide(text: &[u16]) -> String {
    let end = text
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(text.len());
    String::from_utf16_lossy(&text[..end])
}

/// Reads a NUL-terminated C string out of a fixed buffer.
pub(super) fn read_cstr(text: &[c_char]) -> String {
    let bytes: Vec<u8> = text
        .iter()
        .take_while(|unit| **unit != 0)
        .map(|unit| *unit as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

// ---------------------------------------------------------------------------------------------
// IHostApplication

/// The host context plugins receive in `initialize`.
pub(super) struct HostApp;

impl Class for HostApp {
    type Interfaces = (IHostApplication,);
}

/// True when the 16 bytes at `id` are the interface id of `I`.
///
/// # Safety
/// `id` must point at 16 readable bytes.
unsafe fn is_interface<I: Interface>(id: *const c_char) -> bool {
    // SAFETY: the caller guarantees 16 readable bytes; a `Guid` is 16 bytes.
    unsafe { *(id as *const [u8; 16]) == I::IID }
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin passes a valid `String128`.
        write_wide("OpenWave Studio", unsafe { &mut *name });
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the pointers are non-null and name 16-byte ids.
        unsafe {
            if is_interface::<IMessage>(cid as *const c_char) {
                if let Some(message) = ComWrapper::new(Message::default()).to_com_ptr::<IMessage>()
                {
                    let result = message.as_ptr();
                    // Ask for the interface the plugin wants (it is IMessage in practice); the
                    // new reference goes to the plugin.
                    let status = ((*(*(result as *mut FUnknown)).vtbl).queryInterface)(
                        result as *mut FUnknown,
                        iid,
                        obj,
                    );
                    return status;
                }
            } else if is_interface::<IAttributeList>(cid as *const c_char)
                && let Some(list) =
                    ComWrapper::new(AttributeList::default()).to_com_ptr::<IAttributeList>()
            {
                let result = list.as_ptr();
                return ((*(*(result as *mut FUnknown)).vtbl).queryInterface)(
                    result as *mut FUnknown,
                    iid,
                    obj,
                );
            }
        }
        kNotImplemented
    }
}

// ---------------------------------------------------------------------------------------------
// IAttributeList and IMessage

#[derive(Debug, Clone, PartialEq)]
enum Attribute {
    Int(i64),
    Float(f64),
    Text(Vec<u16>),
    Binary(Vec<u8>),
}

/// Key/value store plugins attach to a message.
#[derive(Default)]
pub(super) struct AttributeList {
    values: RefCell<HashMap<String, Attribute>>,
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

/// The attribute id as an owned string.
///
/// # Safety
/// `id` must be null or a NUL-terminated string.
unsafe fn attribute_key(id: *const c_char) -> Option<String> {
    if id.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a NUL-terminated string.
    Some(
        unsafe { std::ffi::CStr::from_ptr(id) }
            .to_string_lossy()
            .into_owned(),
    )
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: *const c_char, value: i64) -> tresult {
        // SAFETY: plugin-supplied attribute id.
        let Some(key) = (unsafe { attribute_key(id) }) else {
            return kInvalidArgument;
        };
        self.values.borrow_mut().insert(key, Attribute::Int(value));
        kResultOk
    }

    unsafe fn getInt(&self, id: *const c_char, value: *mut i64) -> tresult {
        // SAFETY: as above.
        let Some(key) = (unsafe { attribute_key(id) }) else {
            return kInvalidArgument;
        };
        match self.values.borrow().get(&key) {
            Some(Attribute::Int(found)) if !value.is_null() => {
                // SAFETY: non-null out pointer.
                unsafe { *value = *found };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: *const c_char, value: f64) -> tresult {
        // SAFETY: as above.
        let Some(key) = (unsafe { attribute_key(id) }) else {
            return kInvalidArgument;
        };
        self.values
            .borrow_mut()
            .insert(key, Attribute::Float(value));
        kResultOk
    }

    unsafe fn getFloat(&self, id: *const c_char, value: *mut f64) -> tresult {
        // SAFETY: as above.
        let Some(key) = (unsafe { attribute_key(id) }) else {
            return kInvalidArgument;
        };
        match self.values.borrow().get(&key) {
            Some(Attribute::Float(found)) if !value.is_null() => {
                // SAFETY: non-null out pointer.
                unsafe { *value = *found };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: *const c_char, string: *const TChar) -> tresult {
        // SAFETY: as above; `string` is NUL-terminated UTF-16.
        let Some(key) = (unsafe { attribute_key(id) }) else {
            return kInvalidArgument;
        };
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut text = Vec::new();
        // SAFETY: the string is NUL-terminated.
        unsafe {
            let mut at = string;
            while *at != 0 {
                text.push(*at);
                at = at.add(1);
            }
        }
        self.values.borrow_mut().insert(key, Attribute::Text(text));
        kResultOk
    }

    unsafe fn getString(
        &self,
        id: *const c_char,
        string: *mut TChar,
        size_in_bytes: u32,
    ) -> tresult {
        // SAFETY: as above.
        let Some(key) = (unsafe { attribute_key(id) }) else {
            return kInvalidArgument;
        };
        match self.values.borrow().get(&key) {
            Some(Attribute::Text(text)) if !string.is_null() => {
                let capacity = (size_in_bytes as usize) / 2;
                if capacity == 0 {
                    return kResultFalse;
                }
                let length = text.len().min(capacity - 1);
                // SAFETY: the plugin gave `size_in_bytes` of room.
                unsafe {
                    std::ptr::copy_nonoverlapping(text.as_ptr(), string, length);
                    *string.add(length) = 0;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setBinary(
        &self,
        id: *const c_char,
        data: *const c_void,
        size_in_bytes: u32,
    ) -> tresult {
        // SAFETY: as above.
        let Some(key) = (unsafe { attribute_key(id) }) else {
            return kInvalidArgument;
        };
        if data.is_null() && size_in_bytes > 0 {
            return kInvalidArgument;
        }
        let bytes = if size_in_bytes == 0 {
            Vec::new()
        } else {
            // SAFETY: the plugin gave `size_in_bytes` of data.
            unsafe { std::slice::from_raw_parts(data as *const u8, size_in_bytes as usize) }
                .to_vec()
        };
        self.values
            .borrow_mut()
            .insert(key, Attribute::Binary(bytes));
        kResultOk
    }

    unsafe fn getBinary(
        &self,
        id: *const c_char,
        data: *mut *const c_void,
        size_in_bytes: *mut u32,
    ) -> tresult {
        // SAFETY: as above.
        let Some(key) = (unsafe { attribute_key(id) }) else {
            return kInvalidArgument;
        };
        match self.values.borrow().get(&key) {
            Some(Attribute::Binary(bytes)) if !data.is_null() && !size_in_bytes.is_null() => {
                // SAFETY: out pointers are non-null; the bytes live as long as the list (the
                // plugin must copy them before it releases the message, per the SDK).
                unsafe {
                    *data = bytes.as_ptr() as *const c_void;
                    *size_in_bytes = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

/// A message between a plugin's component and controller.
pub(super) struct Message {
    id: RefCell<std::ffi::CString>,
    attributes: ComWrapper<AttributeList>,
    attributes_ptr: ComPtr<IAttributeList>,
}

impl Default for Message {
    fn default() -> Self {
        let attributes = ComWrapper::new(AttributeList::default());
        let attributes_ptr = attributes
            .to_com_ptr::<IAttributeList>()
            .expect("the attribute list implements IAttributeList");
        Self {
            id: RefCell::new(std::ffi::CString::default()),
            attributes,
            attributes_ptr,
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        // The pointer stays valid until the id is set again.
        self.id.borrow().as_ptr()
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        if id.is_null() {
            return;
        }
        // SAFETY: NUL-terminated message id.
        let text = unsafe { std::ffi::CStr::from_ptr(id) }.to_owned();
        *self.id.borrow_mut() = text;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        let _ = &self.attributes;
        self.attributes_ptr.as_ptr()
    }
}

// ---------------------------------------------------------------------------------------------
// IComponentHandler

/// Receives what the controller (and so the plugin's editor) does.
pub(super) struct ComponentHandler {
    shared: Arc<Shared>,
}

impl ComponentHandler {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl ComponentHandler {
    fn current_plain(&self, id: u32) -> f64 {
        match self.shared.controller() {
            // SAFETY: a live controller.
            Some(controller) => unsafe {
                controller.normalizedParamToPlain(id, controller.getParamNormalized(id))
            },
            None => 0.0,
        }
    }
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        self.shared.report(EditorEdit {
            param: id,
            phase: EditPhase::Begin,
            value: self.current_plain(id),
        });
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, value_normalized: ParamValue) -> tresult {
        let value = value_normalized.clamp(0.0, 1.0);
        let plain = self.shared.plain(id, value);
        // The processor must hear the edit too (the host is the only link between the editor
        // and the audio side); a full queue drops it, as for edits from the model.
        let _ = lock(&self.shared.edits).push(Edit { id, value });
        // The model gets the same value back through `set_params`: remembering it here makes
        // that a no-op instead of an echo.
        lock(&self.shared.applied).insert(id, plain);
        self.shared.report(EditorEdit {
            param: id,
            phase: EditPhase::Change,
            value: plain,
        });
        kResultOk
    }

    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        self.shared.report(EditorEdit {
            param: id,
            phase: EditPhase::End,
            value: self.current_plain(id),
        });
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: i32) -> tresult {
        let flags = flags as RestartFlags;
        if flags & (RestartFlags_::kReloadComponent | RestartFlags_::kIoChanged) != 0 {
            self.shared.flags.restart.store(true, Ordering::Release);
        }
        if flags & RestartFlags_::kLatencyChanged != 0 {
            self.shared.flags.latency.store(true, Ordering::Release);
        }
        if flags
            & (RestartFlags_::kParamValuesChanged
                | RestartFlags_::kParamTitlesChanged
                | RestartFlags_::kReloadComponent)
            != 0
        {
            self.shared.flags.params.store(true, Ordering::Release);
        }
        kResultOk
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, state: TBool) -> tresult {
        if state != 0 {
            self.shared.flags.state_dirty.store(true, Ordering::Release);
        }
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, _name: FIDString) -> tresult {
        // The user opens editors from the device panel.
        kResultFalse
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

// ---------------------------------------------------------------------------------------------
// IBStream

/// A stream in memory: what state is saved into and loaded from.
#[derive(Default)]
pub(super) struct MemStream {
    data: RefCell<Vec<u8>>,
    position: Cell<usize>,
}

impl MemStream {
    pub(super) fn reading(bytes: &[u8]) -> Self {
        Self {
            data: RefCell::new(bytes.to_vec()),
            position: Cell::new(0),
        }
    }

    pub(super) fn bytes(&self) -> Vec<u8> {
        self.data.borrow().clone()
    }
}

impl Class for MemStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemStream {
    unsafe fn read(
        &self,
        buffer: *mut c_void,
        num_bytes: i32,
        num_bytes_read: *mut i32,
    ) -> tresult {
        let wanted = usize::try_from(num_bytes).unwrap_or(0);
        let data = self.data.borrow();
        let at = self.position.get().min(data.len());
        let count = wanted.min(data.len() - at);
        if count > 0 {
            if buffer.is_null() {
                return kInvalidArgument;
            }
            // SAFETY: the plugin gave room for `num_bytes`.
            unsafe { std::ptr::copy_nonoverlapping(data[at..].as_ptr(), buffer as *mut u8, count) };
        }
        self.position.set(at + count);
        if !num_bytes_read.is_null() {
            // SAFETY: non-null out pointer.
            unsafe { *num_bytes_read = i32::try_from(count).unwrap_or(i32::MAX) };
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: i32,
        num_bytes_written: *mut i32,
    ) -> tresult {
        let count = usize::try_from(num_bytes).unwrap_or(0);
        if count > 0 && buffer.is_null() {
            return kInvalidArgument;
        }
        let mut data = self.data.borrow_mut();
        let at = self.position.get().min(data.len());
        if count > 0 {
            // SAFETY: the plugin gave `num_bytes` of data.
            let source = unsafe { std::slice::from_raw_parts(buffer as *const u8, count) };
            let overlap = (data.len() - at).min(count);
            data[at..at + overlap].copy_from_slice(&source[..overlap]);
            data.extend_from_slice(&source[overlap..]);
        }
        self.position.set(at + count);
        if !num_bytes_written.is_null() {
            // SAFETY: non-null out pointer.
            unsafe { *num_bytes_written = num_bytes.max(0) };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: i64, mode: i32, result: *mut i64) -> tresult {
        let length = self.data.borrow().len() as i64;
        let target = match mode as IBStream_::IStreamSeekMode {
            IBStream_::IStreamSeekMode_::kIBSeekSet => pos,
            IBStream_::IStreamSeekMode_::kIBSeekCur => self.position.get() as i64 + pos,
            IBStream_::IStreamSeekMode_::kIBSeekEnd => length + pos,
            _ => return kInvalidArgument,
        };
        if target < 0 {
            return kInvalidArgument;
        }
        self.position
            .set(usize::try_from(target).unwrap_or(usize::MAX));
        if !result.is_null() {
            // SAFETY: non-null out pointer.
            unsafe { *result = target };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut i64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: non-null out pointer.
        unsafe { *pos = self.position.get() as i64 };
        kResultOk
    }
}

// ---------------------------------------------------------------------------------------------
// IParameterChanges

/// Points one queue holds (the host adds one per parameter and block; a plugin's own output
/// changes may add a few).
const QUEUE_POINTS: usize = 8;

/// One parameter's changes within a block.
pub(super) struct ParamQueue {
    id: Cell<u32>,
    length: Cell<usize>,
    points: [Cell<(i32, f64)>; QUEUE_POINTS],
}

impl ParamQueue {
    fn new() -> Self {
        Self {
            id: Cell::new(0),
            length: Cell::new(0),
            points: std::array::from_fn(|_| Cell::new((0, 0.0))),
        }
    }
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> i32 {
        self.length.get() as i32
    }

    unsafe fn getPoint(
        &self,
        index: i32,
        sample_offset: *mut i32,
        value: *mut ParamValue,
    ) -> tresult {
        let Ok(index) = usize::try_from(index) else {
            return kResultFalse;
        };
        if index >= self.length.get() || sample_offset.is_null() || value.is_null() {
            return kResultFalse;
        }
        let (offset, at) = self.points[index].get();
        // SAFETY: non-null out pointers.
        unsafe {
            *sample_offset = offset;
            *value = at;
        }
        kResultTrue
    }

    unsafe fn addPoint(&self, sample_offset: i32, value: ParamValue, index: *mut i32) -> tresult {
        let at = self.length.get();
        if at >= QUEUE_POINTS {
            return kResultFalse;
        }
        self.points[at].set((sample_offset, value));
        self.length.set(at + 1);
        if !index.is_null() {
            // SAFETY: non-null out pointer.
            unsafe { *index = at as i32 };
        }
        kResultTrue
    }
}

/// Parameter queues of one block: preallocated, cleared before each block.
pub(super) struct ParamChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    pointers: Vec<ComPtr<IParamValueQueue>>,
    used: Cell<usize>,
}

impl ParamChanges {
    /// A list that holds changes for up to `capacity` parameters per block.
    pub(super) fn new(capacity: usize) -> Self {
        let queues: Vec<ComWrapper<ParamQueue>> = (0..capacity)
            .map(|_| ComWrapper::new(ParamQueue::new()))
            .collect();
        let pointers = queues
            .iter()
            .map(|queue| {
                queue
                    .to_com_ptr::<IParamValueQueue>()
                    .expect("the queue implements IParamValueQueue")
            })
            .collect();
        Self {
            queues,
            pointers,
            used: Cell::new(0),
        }
    }

    pub(super) fn clear(&self) {
        for queue in &self.queues[..self.used.get()] {
            queue.length.set(0);
        }
        self.used.set(0);
    }

    /// Sets the one value `id` has in this block (replacing an earlier one): host edits are
    /// "latest wins" within a block. False when the list is full.
    pub(super) fn set_last(&self, id: u32, offset: i32, value: f64) -> bool {
        let used = self.used.get();
        if let Some(queue) = self.queues[..used]
            .iter()
            .find(|queue| queue.id.get() == id)
        {
            queue.points[0].set((offset, value));
            queue.length.set(1);
            return true;
        }
        self.push(id, offset, value)
    }

    /// Adds a value for `id` at `offset`; false when the list is full.
    pub(super) fn push(&self, id: u32, offset: i32, value: f64) -> bool {
        let used = self.used.get();
        let existing = self.queues[..used]
            .iter()
            .find(|queue| queue.id.get() == id);
        let queue = match existing {
            Some(queue) => queue,
            None => {
                let Some(queue) = self.queues.get(used) else {
                    return false;
                };
                queue.id.set(id);
                queue.length.set(0);
                self.used.set(used + 1);
                queue
            }
        };
        let at = queue.length.get();
        if at >= QUEUE_POINTS {
            return false;
        }
        queue.points[at].set((offset, value));
        queue.length.set(at + 1);
        true
    }
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        self.used.get() as i32
    }

    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        match usize::try_from(index) {
            Ok(index) if index < self.used.get() => self.pointers[index].as_ptr(),
            _ => std::ptr::null_mut(),
        }
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut i32,
    ) -> *mut IParamValueQueue {
        if id.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: non-null id pointer.
        let id = unsafe { *id };
        let used = self.used.get();
        let position = match self.queues[..used]
            .iter()
            .position(|queue| queue.id.get() == id)
        {
            Some(position) => position,
            None => {
                if used >= self.queues.len() {
                    return std::ptr::null_mut();
                }
                self.queues[used].id.set(id);
                self.queues[used].length.set(0);
                self.used.set(used + 1);
                used
            }
        };
        if !index.is_null() {
            // SAFETY: non-null out pointer.
            unsafe { *index = position as i32 };
        }
        self.pointers[position].as_ptr()
    }
}

// ---------------------------------------------------------------------------------------------
// IEventList

/// The note events of one block: preallocated, cleared before each block.
pub(super) struct EventList {
    events: Vec<Cell<Event>>,
    count: Cell<usize>,
}

impl EventList {
    pub(super) fn new(capacity: usize) -> Self {
        // SAFETY: `Event` is plain old data (integers and a union of them): all zeroes is valid.
        let empty: Event = unsafe { std::mem::zeroed() };
        Self {
            events: (0..capacity).map(|_| Cell::new(empty)).collect(),
            count: Cell::new(0),
        }
    }

    pub(super) fn clear(&self) {
        self.count.set(0);
    }

    /// Adds an event; false when the list is full.
    pub(super) fn push(&self, event: Event) -> bool {
        let at = self.count.get();
        let Some(slot) = self.events.get(at) else {
            return false;
        };
        slot.set(event);
        self.count.set(at + 1);
        true
    }
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> i32 {
        self.count.get() as i32
    }

    unsafe fn getEvent(&self, index: i32, e: *mut Event) -> tresult {
        match usize::try_from(index) {
            Ok(index) if index < self.count.get() && !e.is_null() => {
                // SAFETY: non-null out pointer.
                unsafe { *e = self.events[index].get() };
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        if e.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: non-null event from the plugin.
        if self.push(unsafe { *e }) {
            kResultOk
        } else {
            kOutOfMemory
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_reads_what_was_written_and_seeks() {
        let stream = ComWrapper::new(MemStream::default());
        let ptr = stream.to_com_ptr::<IBStream>().unwrap();
        let mut data = *b"openwave";
        let mut written = 0;
        // SAFETY: valid buffers for the calls.
        unsafe {
            assert_eq!(
                ptr.write(data.as_mut_ptr() as *mut c_void, 8, &mut written),
                kResultOk
            );
            assert_eq!(written, 8);
            let mut at = 0;
            assert_eq!(
                ptr.seek(4, IBStream_::IStreamSeekMode_::kIBSeekSet, &mut at),
                kResultOk
            );
            let mut buffer = [0_u8; 8];
            let mut read = 0;
            assert_eq!(
                ptr.read(buffer.as_mut_ptr() as *mut c_void, 8, &mut read),
                kResultOk
            );
            assert_eq!((read, &buffer[..4]), (4, &b"wave"[..]));
            assert_eq!(ptr.tell(&mut at), kResultOk);
            assert_eq!(at, 8);
        }
        assert_eq!(stream.bytes(), b"openwave");
    }

    #[test]
    fn parameter_changes_group_values_by_parameter() {
        let changes = ComWrapper::new(ParamChanges::new(4));
        changes.push(7, 0, 0.25);
        changes.push(9, 0, 0.5);
        changes.push(7, 16, 0.75);
        let ptr = changes.to_com_ptr::<IParameterChanges>().unwrap();
        // SAFETY: the list is alive; the queue pointers are valid while it is.
        unsafe {
            assert_eq!(ptr.getParameterCount(), 2);
            let queue = vst3::ComRef::from_raw(ptr.getParameterData(0)).unwrap();
            assert_eq!((queue.getParameterId(), queue.getPointCount()), (7, 2));
            let (mut offset, mut value) = (0, 0.0);
            assert_eq!(queue.getPoint(1, &mut offset, &mut value), kResultTrue);
            assert_eq!((offset, value), (16, 0.75));
        }
        changes.clear();
        // SAFETY: as above.
        assert_eq!(unsafe { ptr.getParameterCount() }, 0);
    }

    #[test]
    fn the_host_makes_messages_with_attributes() {
        let host = ComWrapper::new(HostApp);
        let app = host.to_com_ptr::<IHostApplication>().unwrap();
        let mut object: *mut c_void = std::ptr::null_mut();
        // SAFETY: valid ids and out pointer; the message is released through its ComPtr.
        unsafe {
            let id = IMessage::IID;
            let result = app.createInstance(
                id.as_ptr() as *mut TUID,
                id.as_ptr() as *mut TUID,
                &mut object,
            );
            assert_eq!(result, kResultOk);
            let message = ComPtr::<IMessage>::from_raw(object as *mut IMessage).unwrap();
            message.setMessageID(c"ping".as_ptr());
            assert_eq!(
                std::ffi::CStr::from_ptr(message.getMessageID())
                    .to_str()
                    .unwrap(),
                "ping"
            );
            let attributes = vst3::ComRef::from_raw(message.getAttributes()).unwrap();
            assert_eq!(attributes.setInt(c"count".as_ptr(), 42), kResultOk);
            let mut value = 0;
            assert_eq!(attributes.getInt(c"count".as_ptr(), &mut value), kResultOk);
            assert_eq!(value, 42);
            assert_eq!(
                attributes.getInt(c"missing".as_ptr(), &mut value),
                kResultFalse
            );
        }
    }
}
