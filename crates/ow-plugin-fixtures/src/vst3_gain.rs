//! The VST3 fixture, written directly against the `vst3` crate like a third-party plugin would be.
//!
//! * **Gain** (`GainProcessor` + `GainController`, two classes): stereo in and out, delayed by
//!   [`GAIN_LATENCY`] frames (which `getLatencySamples` reports); `Gain` (id 1, plain 0 to 2,
//!   normalised plain/2) and `Polarity` (id 2, a two-step list); state in the processor and in
//!   the controller; an editor view (`IPlugView`) that attaches a plain native child view.
//! * **Tone** (`Tone`, one class that is component, processor and controller at once): no audio
//!   input, an event input bus; while a note is held it outputs a constant `Level * velocity`.
//!
//! The `ow_fixture_*` functions at the end are the hooks host tests use to act as the editor's
//! user (see [`crate::hooks`]).

// The SDK's enumeration constants are `c_int` on Windows and `c_uint` elsewhere, so casts that
// are no-ops on one platform are needed on the other.
#![allow(
    non_snake_case,
    non_upper_case_globals,
    non_camel_case_types,
    clippy::unnecessary_cast
)]

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_char, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, uid};

use crate::hooks::{EditorState, PHASE_BEGIN, PHASE_CHANGE};
use crate::view::{ChildView, DEFAULT_SIZE};
use crate::{GAIN_LATENCY, PARAM_GAIN, PARAM_LEVEL, PARAM_POLARITY};

const LATENCY: usize = GAIN_LATENCY as usize;
const STATE_MAGIC: &[u8; 4] = b"OWFX";
const STATE_LEN: usize = 13;
const CONTROLLER_MAGIC: &[u8; 4] = b"OWCT";

/// Class ids; their text forms are `crate::VST3_*_ID`.
const GAIN_CID: TUID = uid(0x4F57_4658, 0x5653_5433, 0x4741_494E, 0x5052_4F43);
const GAIN_CONTROLLER_CID: TUID = uid(0x4F57_4658, 0x5653_5433, 0x4741_494E, 0x4354_4C52);
const TONE_CID: TUID = uid(0x4F57_4658, 0x5653_5433, 0x544F_4E45, 0x5052_4F43);

// ---------------------------------------------------------------------------------------------
// Helpers

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let bytes = src.as_bytes();
    let length = bytes.len().min(dst.len().saturating_sub(1));
    for (slot, byte) in dst.iter_mut().zip(&bytes[..length]) {
        *slot = *byte as c_char;
    }
    if let Some(end) = dst.get_mut(length) {
        *end = 0;
    }
}

fn copy_wstring(src: &str, dst: &mut [u16]) {
    let mut length = 0;
    for (unit, slot) in src.encode_utf16().zip(dst.iter_mut()) {
        *slot = unit;
        length += 1;
    }
    if length < dst.len() {
        dst[length] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

unsafe fn wstring_len(text: *const u16) -> usize {
    let mut length = 0;
    // SAFETY: NUL-terminated by contract.
    unsafe {
        while *text.add(length) != 0 {
            length += 1;
        }
    }
    length
}

/// Everything the host's stream holds from its start.
unsafe fn read_all(state: *mut IBStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    // SAFETY: the host passes a live stream.
    let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
        return bytes;
    };
    let mut buffer = [0_u8; 256];
    loop {
        let mut read = 0;
        // SAFETY: a valid buffer of the stated size.
        let result = unsafe {
            stream.read(
                buffer.as_mut_ptr() as *mut c_void,
                buffer.len() as i32,
                &mut read,
            )
        };
        if result != kResultOk || read <= 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read as usize]);
    }
    bytes
}

unsafe fn write_all(state: *mut IBStream, bytes: &[u8]) -> tresult {
    // SAFETY: the host passes a live stream.
    let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
        return kInvalidArgument;
    };
    let mut written = 0;
    // SAFETY: `bytes` is valid for its length (the host does not write to it).
    unsafe {
        stream.write(
            bytes.as_ptr() as *mut c_void,
            bytes.len() as i32,
            &mut written,
        )
    }
}

/// A pointer to a fixture object that the test hooks may call.
struct Handle<T>(*const T);

// SAFETY: the hooks run on the plugin main thread, like the host's own calls into the plugin.
unsafe impl<T> Send for Handle<T> {}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------------------------
// Gain: processor

struct GainProcessor {
    /// Plain gain as bits (0 to 2).
    gain: AtomicU64,
    inverted: AtomicU32,
    delay: RefCell<[Vec<f32>; 2]>,
    position: Cell<usize>,
}

impl Class for GainProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl GainProcessor {
    fn new() -> Self {
        Self {
            gain: AtomicU64::new(1.0_f64.to_bits()),
            inverted: AtomicU32::new(0),
            delay: RefCell::new([vec![0.0; LATENCY], vec![0.0; LATENCY]]),
            position: Cell::new(0),
        }
    }

    fn gain(&self) -> f64 {
        f64::from_bits(self.gain.load(Ordering::Relaxed))
    }
}

impl IPluginBaseTrait for GainProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

fn stereo_bus(bus: &mut BusInfo, direction: BusDirections, name: &str) {
    bus.mediaType = MediaTypes_::kAudio as MediaType;
    bus.direction = direction as BusDirection;
    bus.channelCount = 2;
    copy_wstring(name, &mut bus.name);
    bus.busType = BusTypes_::kMain as BusType;
    bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
}

impl IComponentTrait for GainProcessor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        // SAFETY: a valid out pointer.
        unsafe { *class_id = GAIN_CONTROLLER_CID };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media_type: MediaType, _dir: BusDirection) -> i32 {
        i32::from(media_type == MediaTypes_::kAudio as MediaType)
    }

    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if media_type != MediaTypes_::kAudio as MediaType || index != 0 {
            return kInvalidArgument;
        }
        // SAFETY: a valid out pointer.
        let bus = unsafe { &mut *bus };
        if dir == BusDirections_::kInput as BusDirection {
            stereo_bus(bus, BusDirections_::kInput, "Input");
        } else {
            stereo_bus(bus, BusDirections_::kOutput, "Output");
        }
        kResultOk
    }

    unsafe fn getRoutingInfo(
        &self,
        _in_info: *mut RoutingInfo,
        _out_info: *mut RoutingInfo,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        _media_type: MediaType,
        _dir: BusDirection,
        _index: i32,
        _state: TBool,
    ) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        if state != 0 {
            let mut delay = self.delay.borrow_mut();
            delay[0].fill(0.0);
            delay[1].fill(0.0);
            self.position.set(0);
        }
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: the host passes a live stream.
        let bytes = unsafe { read_all(state) };
        if bytes.len() != STATE_LEN || &bytes[..4] != STATE_MAGIC {
            return kResultFalse;
        }
        let gain = f64::from_le_bytes(bytes[4..12].try_into().unwrap_or([0; 8]));
        self.gain
            .store(gain.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
        self.inverted.store(u32::from(bytes[12]), Ordering::Relaxed);
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let mut bytes = Vec::with_capacity(STATE_LEN);
        bytes.extend_from_slice(STATE_MAGIC);
        bytes.extend_from_slice(&self.gain().to_le_bytes());
        bytes.push(self.inverted.load(Ordering::Relaxed) as u8);
        // SAFETY: the host passes a live stream.
        unsafe { write_all(state, &bytes) }
    }
}

impl IAudioProcessorTrait for GainProcessor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        // SAFETY: the host passes `num_ins` and `num_outs` arrangements.
        unsafe {
            if num_ins != 1 || num_outs != 1 {
                return kResultFalse;
            }
            if *inputs != SpeakerArr::kStereo || *outputs != SpeakerArr::kStereo {
                return kResultFalse;
            }
        }
        kResultTrue
    }

    unsafe fn getBusArrangement(
        &self,
        _dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        // SAFETY: a valid out pointer.
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, symbolic_sample_size: i32) -> tresult {
        if symbolic_sample_size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kNotImplemented
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        crate::reported_latency()
    }

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        // SAFETY: the host passes valid process data, buffers included.
        unsafe {
            let data = &*data;
            if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
                for index in 0..changes.getParameterCount() {
                    let Some(queue) = ComRef::from_raw(changes.getParameterData(index)) else {
                        continue;
                    };
                    let points = queue.getPointCount();
                    if points == 0 {
                        continue;
                    }
                    let (mut offset, mut value) = (0, 0.0);
                    if queue.getPoint(points - 1, &mut offset, &mut value) != kResultTrue {
                        continue;
                    }
                    match queue.getParameterId() {
                        PARAM_GAIN => self
                            .gain
                            .store((value * 2.0).clamp(0.0, 2.0).to_bits(), Ordering::Relaxed),
                        PARAM_POLARITY => self
                            .inverted
                            .store(u32::from(value >= 0.5), Ordering::Relaxed),
                        _ => {}
                    }
                }
            }
            if data.numInputs != 1 || data.numOutputs != 1 {
                return kResultOk;
            }
            let input = &*data.inputs;
            let output = &*data.outputs;
            if input.numChannels != 2 || output.numChannels != 2 {
                return kResultOk;
            }
            let frames = data.numSamples.max(0) as usize;
            let sign = if self.inverted.load(Ordering::Relaxed) == 1 {
                -1.0
            } else {
                1.0
            };
            let gain = (self.gain() * sign) as f32;
            let in_channels = std::slice::from_raw_parts(input.__field0.channelBuffers32, 2);
            let out_channels = std::slice::from_raw_parts(output.__field0.channelBuffers32, 2);
            let mut delay = self.delay.borrow_mut();
            for channel in 0..2 {
                let samples_in = std::slice::from_raw_parts(in_channels[channel], frames);
                let samples_out = std::slice::from_raw_parts_mut(out_channels[channel], frames);
                let line = &mut delay[channel];
                let mut at = self.position.get();
                for (sample_in, sample_out) in samples_in.iter().zip(samples_out.iter_mut()) {
                    *sample_out = line[at] * gain;
                    line[at] = *sample_in;
                    at = (at + 1) % LATENCY;
                }
            }
            self.position.set((self.position.get() + frames) % LATENCY);
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for GainProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

// ---------------------------------------------------------------------------------------------
// Gain: controller

/// The controller whose editor is open, for the test hooks.
static CONTROLLER: Mutex<Option<Handle<GainController>>> = Mutex::new(None);
/// The editor view that is attached, for the test hooks.
static VIEW: Mutex<Option<Handle<EditorView>>> = Mutex::new(None);

struct GainController {
    /// Normalised values.
    gain: Cell<f64>,
    polarity: Cell<f64>,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
}

impl Class for GainController {
    type Interfaces = (IEditController,);
}

impl GainController {
    fn new() -> Self {
        Self {
            gain: Cell::new(0.5),
            polarity: Cell::new(0.0),
            handler: RefCell::new(None),
        }
    }

    fn set(&self, id: u32, normalized: f64) -> bool {
        let value = normalized.clamp(0.0, 1.0);
        match id {
            PARAM_GAIN => self.gain.set(value),
            PARAM_POLARITY => self.polarity.set(value.round()),
            _ => return false,
        }
        true
    }

    fn plain_to_normalized(id: u32, plain: f64) -> f64 {
        match id {
            PARAM_GAIN => plain / 2.0,
            _ => plain,
        }
    }

    /// The user's gesture (test hook): `phase` as in [`crate::hooks`].
    fn gesture(&self, param: u32, plain: f64, phase: u32) {
        let handler = self.handler.borrow().clone();
        let Some(handler) = handler else {
            return;
        };
        // SAFETY: a live handler given by the host.
        unsafe {
            match phase {
                PHASE_BEGIN => {
                    handler.beginEdit(param);
                }
                PHASE_CHANGE => {
                    let normalized = Self::plain_to_normalized(param, plain);
                    self.set(param, normalized);
                    handler.performEdit(param, normalized);
                }
                _ => {
                    handler.endEdit(param);
                }
            }
        }
    }
}

impl IPluginBaseTrait for GainController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        *lock(&CONTROLLER) = Some(Handle(self as *const GainController));
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        let mut registered = lock(&CONTROLLER);
        if registered
            .as_ref()
            .is_some_and(|handle| std::ptr::eq(handle.0, self))
        {
            *registered = None;
        }
        *self.handler.borrow_mut() = None;
        kResultOk
    }
}

impl IEditControllerTrait for GainController {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: the host passes a live stream.
        let bytes = unsafe { read_all(state) };
        if bytes.len() != STATE_LEN || &bytes[..4] != STATE_MAGIC {
            return kResultFalse;
        }
        let gain = f64::from_le_bytes(bytes[4..12].try_into().unwrap_or([0; 8]));
        self.set(PARAM_GAIN, gain / 2.0);
        self.set(PARAM_POLARITY, f64::from(bytes[12]));
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: the host passes a live stream.
        let bytes = unsafe { read_all(state) };
        if bytes.len() != 4 + 16 || &bytes[..4] != CONTROLLER_MAGIC {
            return kResultFalse;
        }
        self.set(
            PARAM_GAIN,
            f64::from_le_bytes(bytes[4..12].try_into().unwrap_or([0; 8])),
        );
        self.set(
            PARAM_POLARITY,
            f64::from_le_bytes(bytes[12..20].try_into().unwrap_or([0; 8])),
        );
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let mut bytes = Vec::with_capacity(20);
        bytes.extend_from_slice(CONTROLLER_MAGIC);
        bytes.extend_from_slice(&self.gain.get().to_le_bytes());
        bytes.extend_from_slice(&self.polarity.get().to_le_bytes());
        // SAFETY: the host passes a live stream.
        unsafe { write_all(state, &bytes) }
    }

    unsafe fn getParameterCount(&self) -> i32 {
        2
    }

    unsafe fn getParameterInfo(&self, param_index: i32, info: *mut ParameterInfo) -> tresult {
        // SAFETY: a valid out pointer.
        let info = unsafe { &mut *info };
        match param_index {
            0 => {
                info.id = PARAM_GAIN;
                copy_wstring("Gain", &mut info.title);
                copy_wstring("Gain", &mut info.shortTitle);
                copy_wstring("", &mut info.units);
                info.stepCount = 0;
                info.defaultNormalizedValue = 0.5;
                info.unitId = 0;
                info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
                kResultOk
            }
            1 => {
                info.id = PARAM_POLARITY;
                copy_wstring("Polarity", &mut info.title);
                copy_wstring("Polarity", &mut info.shortTitle);
                copy_wstring("", &mut info.units);
                info.stepCount = 1;
                info.defaultNormalizedValue = 0.0;
                info.unitId = 0;
                info.flags = (ParameterInfo_::ParameterFlags_::kCanAutomate
                    | ParameterInfo_::ParameterFlags_::kIsList) as i32;
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn getParamStringByValue(
        &self,
        id: u32,
        value_normalized: f64,
        string: *mut String128,
    ) -> tresult {
        // SAFETY: a valid out pointer.
        let out = unsafe { &mut *string };
        match id {
            PARAM_GAIN => {
                copy_wstring(&format!("{:.2}", value_normalized * 2.0), out);
                kResultOk
            }
            PARAM_POLARITY => {
                copy_wstring(
                    if value_normalized >= 0.5 {
                        "Inverted"
                    } else {
                        "Normal"
                    },
                    out,
                );
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn getParamValueByString(
        &self,
        id: u32,
        string: *mut TChar,
        value_normalized: *mut f64,
    ) -> tresult {
        // SAFETY: NUL-terminated text and a valid out pointer.
        let text = unsafe {
            let length = wstring_len(string);
            String::from_utf16_lossy(std::slice::from_raw_parts(string, length))
        };
        let value = match id {
            PARAM_GAIN => text.trim().parse::<f64>().ok().map(|plain| plain / 2.0),
            PARAM_POLARITY => Some(f64::from(u8::from(text.eq_ignore_ascii_case("inverted")))),
            _ => None,
        };
        match value {
            Some(value) => {
                // SAFETY: a valid out pointer.
                unsafe { *value_normalized = value };
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn normalizedParamToPlain(&self, id: u32, value_normalized: f64) -> f64 {
        match id {
            PARAM_GAIN => value_normalized * 2.0,
            _ => value_normalized,
        }
    }

    unsafe fn plainParamToNormalized(&self, id: u32, plain_value: f64) -> f64 {
        Self::plain_to_normalized(id, plain_value)
    }

    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        match id {
            PARAM_GAIN => self.gain.get(),
            PARAM_POLARITY => self.polarity.get(),
            _ => 0.0,
        }
    }

    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        if self.set(id, value) {
            kResultOk
        } else {
            kInvalidArgument
        }
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        // SAFETY: a live handler, or null to release it.
        *self.handler.borrow_mut() =
            unsafe { ComRef::from_raw(handler) }.map(|handler| handler.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, name: *const c_char) -> *mut IPlugView {
        // SAFETY: a NUL-terminated view name.
        if name.is_null() || unsafe { CStr::from_ptr(name) } != c"editor" {
            return std::ptr::null_mut();
        }
        let view = ComWrapper::new(EditorView::new());
        let Some(pointer) = view.to_com_ptr::<IPlugView>() else {
            return std::ptr::null_mut();
        };
        view.self_pointer.set(pointer.as_ptr());
        pointer.into_raw()
    }
}

// ---------------------------------------------------------------------------------------------
// Gain: editor view

struct EditorView {
    child: RefCell<Option<ChildView>>,
    size: Cell<(u32, u32)>,
    frame: RefCell<Option<ComPtr<IPlugFrame>>>,
    self_pointer: Cell<*mut IPlugView>,
}

impl Class for EditorView {
    type Interfaces = (IPlugView,);
}

impl EditorView {
    fn new() -> Self {
        Self {
            child: RefCell::new(None),
            size: Cell::new(DEFAULT_SIZE),
            frame: RefCell::new(None),
            self_pointer: Cell::new(std::ptr::null_mut()),
        }
    }

    fn platform_type() -> &'static CStr {
        if cfg!(windows) {
            c"HWND"
        } else if cfg!(target_os = "macos") {
            c"NSView"
        } else {
            c"unsupported"
        }
    }
}

impl IPlugViewTrait for EditorView {
    unsafe fn isPlatformTypeSupported(&self, r#type: FIDString) -> tresult {
        // SAFETY: a NUL-terminated platform name.
        if cfg!(any(windows, target_os = "macos"))
            && !r#type.is_null()
            && unsafe { CStr::from_ptr(r#type) } == Self::platform_type()
        {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn attached(&self, parent: *mut c_void, r#type: FIDString) -> tresult {
        // SAFETY: as above.
        if unsafe { self.isPlatformTypeSupported(r#type) } != kResultTrue {
            return kResultFalse;
        }
        let (width, height) = self.size.get();
        let Some(child) = ChildView::create(parent, width, height) else {
            return kResultFalse;
        };
        *self.child.borrow_mut() = Some(child);
        *lock(&VIEW) = Some(Handle(self as *const EditorView));
        kResultOk
    }

    unsafe fn removed(&self) -> tresult {
        *self.child.borrow_mut() = None;
        let mut registered = lock(&VIEW);
        if registered
            .as_ref()
            .is_some_and(|handle| std::ptr::eq(handle.0, self))
        {
            *registered = None;
        }
        kResultOk
    }

    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyDown(&self, _key: char16, _key_code: i16, _modifiers: i16) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyUp(&self, _key: char16, _key_code: i16, _modifiers: i16) -> tresult {
        kResultFalse
    }

    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        let (width, height) = self.size.get();
        // SAFETY: a valid out pointer.
        unsafe {
            *size = ViewRect {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
        }
        kResultOk
    }

    unsafe fn onSize(&self, new_size: *mut ViewRect) -> tresult {
        // SAFETY: a valid rectangle.
        let rect = unsafe { *new_size };
        let size = (
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
        );
        self.size.set(size);
        if let Some(child) = self.child.borrow().as_ref() {
            child.set_size(size.0, size.1);
        }
        kResultOk
    }

    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        // SAFETY: a live frame, or null to release it.
        *self.frame.borrow_mut() =
            unsafe { ComRef::from_raw(frame) }.map(|frame| frame.to_com_ptr());
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        kResultTrue
    }

    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        // SAFETY: a valid rectangle.
        unsafe {
            let rect = &mut *rect;
            rect.right = rect.left + (rect.right - rect.left).max(100);
            rect.bottom = rect.top + (rect.bottom - rect.top).max(80);
        }
        kResultOk
    }
}

// ---------------------------------------------------------------------------------------------
// Tone: component, processor and controller in one object

struct Tone {
    level: AtomicU64,
    /// The velocity of the held note, 0 when none is held.
    held: Cell<f32>,
}

impl Class for Tone {
    type Interfaces = (
        IComponent,
        IAudioProcessor,
        IEditController,
        IProcessContextRequirements,
    );
}

impl Tone {
    fn new() -> Self {
        Self {
            level: AtomicU64::new(0.5_f64.to_bits()),
            held: Cell::new(0.0),
        }
    }

    fn level(&self) -> f64 {
        f64::from_bits(self.level.load(Ordering::Relaxed))
    }
}

impl IPluginBaseTrait for Tone {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for Tone {
    unsafe fn getControllerClassId(&self, _class_id: *mut TUID) -> tresult {
        // The component is its own controller.
        kNotImplemented
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media_type: MediaType, dir: BusDirection) -> i32 {
        let output = dir == BusDirections_::kOutput as BusDirection;
        if media_type == MediaTypes_::kAudio as MediaType {
            i32::from(output)
        } else if media_type == MediaTypes_::kEvent as MediaType {
            i32::from(!output)
        } else {
            0
        }
    }

    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        // SAFETY: a valid out pointer.
        let bus = unsafe { &mut *bus };
        if media_type == MediaTypes_::kAudio as MediaType
            && dir == BusDirections_::kOutput as BusDirection
        {
            stereo_bus(bus, BusDirections_::kOutput, "Output");
            kResultOk
        } else if media_type == MediaTypes_::kEvent as MediaType
            && dir == BusDirections_::kInput as BusDirection
        {
            bus.mediaType = MediaTypes_::kEvent as MediaType;
            bus.direction = BusDirections_::kInput as BusDirection;
            bus.channelCount = 1;
            copy_wstring("Notes", &mut bus.name);
            bus.busType = BusTypes_::kMain as BusType;
            bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
            kResultOk
        } else {
            kInvalidArgument
        }
    }

    unsafe fn getRoutingInfo(
        &self,
        _in_info: *mut RoutingInfo,
        _out_info: *mut RoutingInfo,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        _media_type: MediaType,
        _dir: BusDirection,
        _index: i32,
        _state: TBool,
    ) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        self.held.set(0.0);
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
}

impl IAudioProcessorTrait for Tone {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        let _ = inputs;
        // SAFETY: the host passes `num_outs` arrangements.
        if num_ins == 0 && num_outs == 1 && unsafe { *outputs } == SpeakerArr::kStereo {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn getBusArrangement(
        &self,
        dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if dir != BusDirections_::kOutput as BusDirection || index != 0 {
            return kInvalidArgument;
        }
        // SAFETY: a valid out pointer.
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, symbolic_sample_size: i32) -> tresult {
        if symbolic_sample_size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kNotImplemented
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        // SAFETY: the host passes valid process data, buffers included.
        unsafe {
            let data = &*data;
            if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
                for index in 0..changes.getParameterCount() {
                    let Some(queue) = ComRef::from_raw(changes.getParameterData(index)) else {
                        continue;
                    };
                    let points = queue.getPointCount();
                    let (mut offset, mut value) = (0, 0.0);
                    if points > 0
                        && queue.getPoint(points - 1, &mut offset, &mut value) == kResultTrue
                        && queue.getParameterId() == PARAM_LEVEL
                    {
                        self.level
                            .store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                    }
                }
            }
            // Note changes as (frame, velocity), in order.
            let mut changes = [(0_usize, 0.0_f32); 64];
            let mut count = 0;
            if let Some(events) = ComRef::from_raw(data.inputEvents) {
                for index in 0..events.getEventCount() {
                    let mut event: Event = std::mem::zeroed();
                    if events.getEvent(index, &mut event) != kResultOk || count >= changes.len() {
                        continue;
                    }
                    let frame = event.sampleOffset.max(0) as usize;
                    if event.r#type == Event_::EventTypes_::kNoteOnEvent as u16 {
                        changes[count] = (frame, event.__field0.noteOn.velocity);
                        count += 1;
                    } else if event.r#type == Event_::EventTypes_::kNoteOffEvent as u16 {
                        changes[count] = (frame, 0.0);
                        count += 1;
                    }
                }
            }
            if data.numOutputs != 1 {
                return kResultOk;
            }
            let output = &*data.outputs;
            let frames = data.numSamples.max(0) as usize;
            let level = self.level() as f32;
            let channels = std::slice::from_raw_parts(
                output.__field0.channelBuffers32,
                output.numChannels as usize,
            );
            let start = self.held.get();
            let mut end = start;
            for channel in channels {
                let samples = std::slice::from_raw_parts_mut(*channel, frames);
                let mut held = start;
                let mut next = 0;
                for (frame, sample) in samples.iter_mut().enumerate() {
                    while next < count && changes[next].0 <= frame {
                        held = changes[next].1;
                        next += 1;
                    }
                    *sample = held * level;
                }
                end = held;
            }
            self.held.set(end);
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for Tone {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

impl IEditControllerTrait for Tone {
    unsafe fn setComponentState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> i32 {
        1
    }

    unsafe fn getParameterInfo(&self, param_index: i32, info: *mut ParameterInfo) -> tresult {
        if param_index != 0 {
            return kInvalidArgument;
        }
        // SAFETY: a valid out pointer.
        let info = unsafe { &mut *info };
        info.id = PARAM_LEVEL;
        copy_wstring("Level", &mut info.title);
        copy_wstring("Level", &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.stepCount = 0;
        info.defaultNormalizedValue = 0.5;
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
        kResultOk
    }

    unsafe fn getParamStringByValue(
        &self,
        _id: u32,
        value_normalized: f64,
        string: *mut String128,
    ) -> tresult {
        // SAFETY: a valid out pointer.
        copy_wstring(&format!("{value_normalized:.2}"), unsafe { &mut *string });
        kResultOk
    }

    unsafe fn getParamValueByString(
        &self,
        _id: u32,
        string: *mut TChar,
        value_normalized: *mut f64,
    ) -> tresult {
        // SAFETY: NUL-terminated text and a valid out pointer.
        unsafe {
            let length = wstring_len(string);
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(string, length));
            match text.trim().parse::<f64>() {
                Ok(value) => {
                    *value_normalized = value;
                    kResultOk
                }
                Err(_) => kInvalidArgument,
            }
        }
    }

    unsafe fn normalizedParamToPlain(&self, _id: u32, value_normalized: f64) -> f64 {
        value_normalized
    }

    unsafe fn plainParamToNormalized(&self, _id: u32, plain_value: f64) -> f64 {
        plain_value
    }

    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        if id == PARAM_LEVEL { self.level() } else { 0.0 }
    }

    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        if id == PARAM_LEVEL {
            self.level
                .store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
            kResultOk
        } else {
            kInvalidArgument
        }
    }

    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
        kResultOk
    }

    unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
        std::ptr::null_mut()
    }
}

// ---------------------------------------------------------------------------------------------
// Factory

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory, IPluginFactory2, IPluginFactory3);
}

const VENDOR: &str = "OpenWave Studio tests";
const NAME: &str = "OpenWave Fixture Gain VST3";
const TONE_NAME: &str = "OpenWave Fixture Tone VST3";

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        // SAFETY: a valid out pointer.
        let info = unsafe { &mut *info };
        copy_cstring(VENDOR, &mut info.vendor);
        copy_cstring(
            "https://github.com/Densetsu152637/OpenWave-Studio",
            &mut info.url,
        );
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as i32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        3
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        // SAFETY: a valid out pointer.
        let info = unsafe { &mut *info };
        let (cid, category, name) = match index {
            0 => (GAIN_CID, "Audio Module Class", NAME),
            1 => (GAIN_CONTROLLER_CID, "Component Controller Class", NAME),
            2 => (TONE_CID, "Audio Module Class", TONE_NAME),
            _ => return kInvalidArgument,
        };
        info.cid = cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_cstring(category, &mut info.category);
        copy_cstring(name, &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        // SAFETY: the host passes 16-byte ids.
        let instance = unsafe {
            match *(cid as *const TUID) {
                GAIN_CID => ComWrapper::new(GainProcessor::new()).to_com_ptr::<FUnknown>(),
                GAIN_CONTROLLER_CID => {
                    ComWrapper::new(GainController::new()).to_com_ptr::<FUnknown>()
                }
                TONE_CID => ComWrapper::new(Tone::new()).to_com_ptr::<FUnknown>(),
                _ => None,
            }
        };
        let Some(instance) = instance else {
            return kInvalidArgument;
        };
        let pointer = instance.as_ptr();
        // SAFETY: a live object; the new reference goes to the host.
        unsafe { ((*(*pointer).vtbl).queryInterface)(pointer, iid as *mut TUID, obj) }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        // SAFETY: a valid out pointer.
        let info = unsafe { &mut *info };
        let (cid, category, name, sub_categories) = match index {
            0 => (GAIN_CID, "Audio Module Class", NAME, "Fx"),
            1 => (GAIN_CONTROLLER_CID, "Component Controller Class", NAME, ""),
            2 => (
                TONE_CID,
                "Audio Module Class",
                TONE_NAME,
                "Instrument|Synth",
            ),
            _ => return kInvalidArgument,
        };
        info.cid = cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_cstring(category, &mut info.category);
        copy_cstring(name, &mut info.name);
        info.classFlags = 0;
        copy_cstring(sub_categories, &mut info.subCategories);
        copy_cstring(VENDOR, &mut info.vendor);
        copy_cstring("1.0.0", &mut info.version);
        copy_cstring("VST 3.7", &mut info.sdkVersion);
        kResultOk
    }
}

impl IPluginFactory3Trait for Factory {
    unsafe fn getClassInfoUnicode(&self, _index: i32, _info: *mut PClassInfoW) -> tresult {
        kNotImplemented
    }

    unsafe fn setHostContext(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
}

// ---------------------------------------------------------------------------------------------
// Module entry points

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn ExitDll() -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn BundleEntry(_bundle_ref: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn BundleExit() -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleEntry(_library_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleExit() -> bool {
    true
}

#[unsafe(no_mangle)]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .map_or(std::ptr::null_mut(), ComPtr::into_raw)
}

// ---------------------------------------------------------------------------------------------
// Test hooks

/// Test hook: the user's gesture on a parameter in the open editor (the controller reports it to
/// the host's component handler). Call it on the plugin main thread.
#[unsafe(no_mangle)]
pub extern "C" fn ow_fixture_simulate_edit(param: u32, value: f64, phase: u32) {
    let controller = lock(&CONTROLLER).as_ref().map(|handle| handle.0);
    if let Some(controller) = controller {
        // SAFETY: registered by `initialize` and cleared by `terminate`, on the main thread.
        unsafe { (*controller).gesture(param, value, phase) };
    }
}

/// Test hook: the editor asks its frame for a new size. Returns 1 when the host accepted.
#[unsafe(no_mangle)]
pub extern "C" fn ow_fixture_request_resize(width: u32, height: u32) -> u32 {
    let view = lock(&VIEW).as_ref().map(|handle| handle.0);
    let Some(view) = view else {
        return 0;
    };
    // SAFETY: registered by `attached` and cleared by `removed`, on the main thread.
    let view = unsafe { &*view };
    let frame = view.frame.borrow().clone();
    let Some(frame) = frame else {
        return 0;
    };
    let mut rect = ViewRect {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    // SAFETY: a live frame and view.
    let result = unsafe { frame.resizeView(view.self_pointer.get(), &mut rect) };
    u32::from(result == kResultOk)
}

/// Test hook: whether the editor's view is attached and how big it is.
///
/// # Safety
/// `out` must be null or point at an `EditorState`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ow_fixture_editor_state(out: *mut EditorState) {
    if out.is_null() {
        return;
    }
    let view = lock(&VIEW).as_ref().map(|handle| handle.0);
    let state = match view {
        // SAFETY: registered by `attached` and cleared by `removed`.
        Some(view) => match unsafe { &*view }.child.borrow().as_ref() {
            Some(child) => {
                let (width, height) = child.size();
                EditorState {
                    attached: 1,
                    width,
                    height,
                }
            }
            None => EditorState::default(),
        },
        None => EditorState::default(),
    };
    // SAFETY: the caller guarantees `out`.
    unsafe { *out = state };
}
