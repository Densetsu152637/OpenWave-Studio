//! The control-side object of a VST3 instance and the scanner's description of a VST3 file.

#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use vst3::ComWrapper;
use vst3::Steinberg::FUnknown;
use vst3::Steinberg::Vst::*;

use super::audio::{AudioSlot, SharedSlot, Vst3Processor};
use super::host::{Edit, HostApp, Shared, lock};
use super::module::{self, AUDIO_CATEGORY, ClassInfo, cid_text};
use super::{EDIT_QUEUE, INSTANCES, Main, NEXT_KEY, create_main, with_main};
use crate::executor::{self, MainThreadExecutor};
use crate::hosted::{EditSink, HostedPlugin, PluginError, PluginProcessor, Requests};
use crate::info::{Format, PluginInfo, kind_of};

/// One VST3 plugin instance.
pub struct Vst3Plugin {
    key: u64,
    info: PluginInfo,
    executor: Arc<dyn MainThreadExecutor>,
    slot: SharedSlot,
    shared: Arc<Shared>,
    latency: Arc<AtomicU32>,
    /// (sample rate, max block) the instance is activated for.
    config: Mutex<(u32, usize)>,
    editor_open: Arc<AtomicBool>,
    destroyed: AtomicBool,
}

impl Vst3Plugin {
    /// Creates and activates the plugin `info` describes, restoring `state` when given.
    pub fn create(
        executor: Arc<dyn MainThreadExecutor>,
        info: PluginInfo,
        sample_rate: u32,
        max_block: usize,
        state: Option<Vec<u8>>,
    ) -> Result<Arc<Self>, PluginError> {
        let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
        let (producer, consumer) = rtrb::RingBuffer::new(EDIT_QUEUE);
        let shared = Shared::new(producer);
        let path = PathBuf::from(&info.path);
        let id = info.id.clone();
        let created = {
            let shared = Arc::clone(&shared);
            executor::call(executor.as_ref(), move || {
                let mut main = create_main(&path, &id, shared)?;
                if let Some(state) = &state {
                    // A state the plugin cannot read leaves it at its defaults; not fatal.
                    let _ = main.load_state(state);
                }
                let side = main.activate(sample_rate, max_block)?;
                let latency = main.latency();
                INSTANCES.with(|instances| instances.borrow_mut().insert(key, main));
                Ok::<_, String>((side, latency))
            })?
        };
        let (side, latency) = created.map_err(PluginError::Plugin)?;
        let slot = Arc::new(Mutex::new(AudioSlot {
            side: Some(side),
            edits: consumer,
        }));
        // On macOS the standard mutex allocates its pthread mutex the first time it is locked.
        // Lock it once here, on the control thread, so the audio thread's first `try_lock`
        // allocates nothing.
        drop(lock(&slot));
        Ok(Arc::new(Self {
            key,
            info,
            executor,
            slot,
            shared,
            latency: Arc::new(AtomicU32::new(latency)),
            config: Mutex::new((sample_rate, max_block)),
            editor_open: Arc::new(AtomicBool::new(false)),
            destroyed: AtomicBool::new(false),
        }))
    }

    fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, PluginError> {
        if self.destroyed.load(Ordering::Acquire) {
            return Err(PluginError::Destroyed);
        }
        Ok(executor::call(self.executor.as_ref(), work)?)
    }
}

/// Re-activates the instance for the given configuration: stops the audio side, sets the plugin
/// up again and gives it a new audio side. Main thread only; `slot` is locked by the caller.
fn reactivate(main: &mut Main, slot: &mut AudioSlot, config: (u32, usize)) -> Result<(), String> {
    let old = slot.side.take();
    main.deactivate(old);
    slot.side = Some(main.activate(config.0, config.1)?);
    Ok(())
}

impl HostedPlugin for Vst3Plugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn processor(&self) -> Box<dyn PluginProcessor> {
        Box::new(Vst3Processor {
            slot: Arc::clone(&self.slot),
            latency: Arc::clone(&self.latency),
            instrument: self.info.instrument,
        })
    }

    fn latency_frames(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    fn set_params(&self, params: &BTreeMap<u32, f64>) -> usize {
        let mut changes: Vec<(u32, f64)> = Vec::new();
        {
            let mut applied = lock(&self.shared.applied);
            for param in &self.info.params {
                if param.read_only {
                    continue;
                }
                let wanted = match params.get(&param.id) {
                    Some(value) => Some(param.clamp(*value)),
                    // Set before, unset now: back to the default.
                    None if applied.contains_key(&param.id) => Some(param.default),
                    None => None,
                };
                let Some(wanted) = wanted else {
                    continue;
                };
                if applied.get(&param.id) == Some(&wanted) {
                    continue;
                }
                applied.insert(param.id, wanted);
                changes.push((param.id, wanted));
            }
            // Unset parameters that are back at their default need no memory.
            applied.retain(|id, value| {
                params.contains_key(id) || self.info.param(*id).is_none_or(|p| p.default != *value)
            });
        }
        if changes.is_empty() {
            return 0;
        }
        let count = changes.len();
        let key = self.key;
        let shared = Arc::clone(&self.shared);
        // Converting plain values to normalised ones is the controller's job, so this is a
        // main-thread call; it also keeps the controller (and an open editor) in step.
        let _ = self.call(move || {
            let _ = with_main(key, |main| {
                for (id, plain) in changes {
                    let normalized = shared.normalized(id, plain);
                    if let Some(controller) = &main.controller {
                        // SAFETY: a live controller.
                        unsafe { controller.setParamNormalized(id, normalized) };
                    }
                    let _ = lock(&shared.edits).push(Edit {
                        id,
                        value: normalized,
                    });
                }
            });
        });
        count
    }

    fn param_value(&self, id: u32) -> Result<Option<f64>, PluginError> {
        let key = self.key;
        self.call(move || {
            with_main(key, |main| {
                let controller = main.controller.as_ref()?;
                // SAFETY: a live controller.
                unsafe {
                    Some(controller.normalizedParamToPlain(id, controller.getParamNormalized(id)))
                }
            })
        })?
    }

    fn save_state(&self) -> Result<Vec<u8>, PluginError> {
        let key = self.key;
        self.call(move || with_main(key, |main| main.save_state())?)?
    }

    fn load_state(&self, state: &[u8]) -> Result<(), PluginError> {
        let key = self.key;
        let state = state.to_vec();
        self.call(move || with_main(key, |main| main.load_state(&state))?)?
    }

    fn service(&self) -> Requests {
        if self.destroyed.load(Ordering::Acquire) {
            return Requests::default();
        }
        let editor = self.editor_open.load(Ordering::Acquire);
        if !self.shared.flags.any() && !editor {
            // Nothing was asked: no main-thread call.
            return Requests::default();
        }
        let key = self.key;
        let shared = Arc::clone(&self.shared);
        let latency = Arc::clone(&self.latency);
        let slot = Arc::clone(&self.slot);
        let editor_open = Arc::clone(&self.editor_open);
        let config = *lock(&self.config);
        let served = self.call(move || {
            let mut requests = Requests::default();
            let _ = with_main(key, |main| {
                if editor && !super::view::poll(&main.editor) {
                    editor_open.store(false, Ordering::Release);
                }
                if shared.flags.restart.swap(false, Ordering::AcqRel) {
                    // The plugin's buses or latency changed: set it up again.
                    let mut guard = lock(&slot);
                    let _ = reactivate(main, &mut guard, config);
                    drop(guard);
                    shared.flags.latency.store(true, Ordering::Release);
                }
                if shared.flags.latency.swap(false, Ordering::AcqRel) {
                    let reported = main.latency();
                    if latency.swap(reported, Ordering::AcqRel) != reported {
                        requests.latency_changed = true;
                    }
                }
                requests.state_dirty = shared.flags.state_dirty.swap(false, Ordering::AcqRel);
                requests.params_rescanned = shared.flags.params.swap(false, Ordering::AcqRel);
            });
            requests
        });
        served.unwrap_or_default()
    }

    fn configure(&self, sample_rate: u32, max_block: usize) -> Result<(), PluginError> {
        {
            let mut config = lock(&self.config);
            if *config == (sample_rate, max_block) {
                return Ok(());
            }
            *config = (sample_rate, max_block);
        }
        let key = self.key;
        let slot = Arc::clone(&self.slot);
        let latency = Arc::clone(&self.latency);
        self.call(move || {
            with_main(key, |main| {
                let mut guard = lock(&slot);
                reactivate(main, &mut guard, (sample_rate, max_block))
                    .map_err(PluginError::Plugin)?;
                drop(guard);
                latency.store(main.latency(), Ordering::Release);
                Ok(())
            })?
        })?
    }

    fn set_edit_sink(&self, sink: EditSink) {
        *lock(&self.shared.sink) = Some(sink);
    }

    fn open_editor(&self) -> Result<(), PluginError> {
        if !crate::editor::AVAILABLE {
            return Err(PluginError::Plugin(
                "plugin editor windows are not available on this platform".into(),
            ));
        }
        let key = self.key;
        let title = self.info.name.clone();
        self.call(move || {
            with_main(key, |main| {
                let controller = main
                    .controller
                    .clone()
                    .ok_or_else(|| "the plugin has no editor".to_string())?;
                super::view::open(&main.editor, &controller, &title)
            })?
            .map_err(PluginError::Plugin)
        })??;
        self.editor_open.store(true, Ordering::Release);
        Ok(())
    }

    fn close_editor(&self) {
        let key = self.key;
        let _ = self.call(move || {
            let _ = with_main(key, |main| super::view::close(&main.editor));
        });
        self.editor_open.store(false, Ordering::Release);
    }

    fn editor_open(&self) -> bool {
        self.editor_open.load(Ordering::Acquire)
    }

    fn editor_size(&self) -> Option<(u32, u32)> {
        let key = self.key;
        self.call(move || with_main(key, |main| main.editor.size()).ok().flatten())
            .ok()
            .flatten()
    }

    fn destroy(&self) {
        if self.destroyed.swap(true, Ordering::AcqRel) {
            return;
        }
        let key = self.key;
        let slot = Arc::clone(&self.slot);
        let torn_down = executor::call(self.executor.as_ref(), move || {
            let main = INSTANCES.with(|instances| instances.borrow_mut().remove(&key));
            // Taking the audio side first makes any node that still runs output silence.
            let side = lock(&slot).side.take();
            if let Some(mut main) = main {
                // Closes the editor, deactivates and terminates the plugin.
                main.deactivate(side);
                main.teardown();
            }
        });
        self.editor_open.store(false, Ordering::Release);
        if torn_down.is_err() {
            eprintln!(
                "[openwave] a plugin instance could not be released: the main thread is gone"
            );
        }
    }
}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        self.destroy();
    }
}

// ---------------------------------------------------------------------------------------------
// Scanning

/// Lists the plugins in the VST3 module at `path`, instantiating each one to read its buses,
/// parameters and latency. Runs on the calling thread, which must be the executor's main thread
/// (the scanner child uses an [`InlineExecutor`](crate::executor::InlineExecutor)). A plugin that
/// crashes here takes the process with it: scan in a child.
pub fn describe_file(
    _executor: &Arc<dyn MainThreadExecutor>,
    path: &Path,
) -> Result<Vec<PluginInfo>, String> {
    let module = module::load(path)?;
    let host = ComWrapper::new(HostApp);
    if let Some(context) = host.to_com_ptr::<FUnknown>() {
        module.set_host_context(context.as_ptr());
    }
    let classes: Vec<ClassInfo> = module
        .classes()
        .into_iter()
        .filter(|class| class.category == AUDIO_CATEGORY)
        .collect();
    if classes.is_empty() {
        return Err("the module has no VST3 audio plugin classes".into());
    }
    let mut found = Vec::new();
    let mut first_error = None;
    for class in &classes {
        match describe_class(path, class) {
            Ok(info) => found.push(info),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    if found.is_empty() {
        Err(first_error.unwrap_or_else(|| "no plugin could be created".into()))
    } else {
        Ok(found)
    }
}

fn describe_class(path: &Path, class: &ClassInfo) -> Result<PluginInfo, String> {
    let id = cid_text(&class.cid);
    let (producer, _consumer) = rtrb::RingBuffer::new(1);
    let mut main = create_main(path, &id, Shared::new(producer))?;
    let params = main.params();
    let mut latency = 0;
    if let Ok(side) = main.activate(48_000, 512) {
        latency = main.latency();
        main.deactivate(Some(side));
    }
    let layout = main.layout;
    let features: Vec<String> = class
        .sub_categories
        .split('|')
        .filter(|feature| !feature.is_empty())
        .map(str::to_string)
        .collect();
    let instrument = features.iter().any(|feature| feature == "Instrument");
    let editor = main.controller.is_some();
    main.teardown();
    Ok(PluginInfo {
        format: Format::Vst3,
        kind: kind_of(Format::Vst3, &id),
        name: if class.name.is_empty() {
            id.clone()
        } else {
            class.name.clone()
        },
        id,
        vendor: class.vendor.clone(),
        version: class.version.clone(),
        features,
        instrument,
        audio_inputs: u32::try_from(layout.in_channels).unwrap_or(0),
        audio_outputs: u32::try_from(layout.out_channels).unwrap_or(0),
        note_input: layout.note_input,
        latency,
        params,
        editor,
        path: std::path::absolute(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned(),
    })
}
