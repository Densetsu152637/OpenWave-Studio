//! [`PluginCache`]: the hosted plugin instances of the running app, keyed by device id.
//!
//! This is to plugins what `SourceCache` is to loaded audio: shared by every compile, so a
//! recompile finds the plugin it built last time instead of making another. An instance is
//! created the first time a device of kind `clap:<id>` is compiled and lives until its device
//! leaves the project or changes kind; parameter edits and recompiles never re-instantiate it.
//! Cloning a cache shares it.
//!
//! The cache also holds the [`PluginDirectory`]: the plugins the scanner found, which is how a
//! kind string becomes a file to load.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use crate::clap::ClapPlugin;
use crate::executor::MainThreadExecutor;
use crate::hosted::{HostedPlugin, PluginError, Requests};
use crate::info::{Format, PluginInfo};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The plugins the scanner found, by kind. Shared (and updated in place) by every clone of a
/// cache and by its forks.
#[derive(Debug, Default)]
pub struct PluginDirectory {
    plugins: RwLock<Vec<PluginInfo>>,
}

impl PluginDirectory {
    /// Replaces the list.
    pub fn set(&self, plugins: Vec<PluginInfo>) {
        *self
            .plugins
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = plugins;
    }

    /// Every plugin, in scan order.
    pub fn all(&self) -> Vec<PluginInfo> {
        self.plugins
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The plugin with `kind` (the first when two files provide the same plugin).
    pub fn find(&self, kind: &str) -> Option<PluginInfo> {
        self.plugins
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|plugin| plugin.kind == kind)
            .cloned()
    }
}

struct Entry {
    kind: String,
    plugin: Arc<dyn HostedPlugin>,
}

struct Inner {
    executor: Arc<dyn MainThreadExecutor>,
    directory: Arc<PluginDirectory>,
    entries: Mutex<HashMap<u64, Entry>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for (_, entry) in lock(&self.entries).drain() {
            entry.plugin.destroy();
        }
    }
}

/// Hosted plugin instances by device id.
#[derive(Clone)]
pub struct PluginCache {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for PluginCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginCache")
            .field("instances", &lock(&self.inner.entries).len())
            .finish()
    }
}

impl PluginCache {
    /// An empty cache whose plugins run their main-thread calls on `executor`.
    pub fn new(executor: Arc<dyn MainThreadExecutor>) -> Self {
        Self {
            inner: Arc::new(Inner {
                executor,
                directory: Arc::new(PluginDirectory::default()),
                entries: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The plugin main thread.
    pub fn executor(&self) -> &Arc<dyn MainThreadExecutor> {
        &self.inner.executor
    }

    /// The plugins the scanner found.
    pub fn directory(&self) -> &Arc<PluginDirectory> {
        &self.inner.directory
    }

    /// A cache with the same executor and directory but no instances: what an offline render
    /// uses, so it never touches the plugins the live engine is running. Dropping it releases
    /// the instances it made.
    pub fn fork(&self) -> PluginCache {
        PluginCache {
            inner: Arc::new(Inner {
                executor: Arc::clone(&self.inner.executor),
                directory: Arc::clone(&self.inner.directory),
                entries: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The instance for device `device` of plugin `kind`, created on first use and reused (and
    /// kept up to date) afterwards. A new instance restores `state` (the plugin's own state
    /// bytes) before anything else; an existing one keeps the state it has. Either way the
    /// model's `params` are applied through the parameter queue.
    pub fn acquire(
        &self,
        device: u64,
        kind: &str,
        params: &BTreeMap<u32, f64>,
        state: Option<Vec<u8>>,
        sample_rate: u32,
        max_block: usize,
    ) -> Result<Arc<dyn HostedPlugin>, PluginError> {
        let mut entries = lock(&self.inner.entries);
        if let Some(entry) = entries.get(&device) {
            if entry.kind == kind {
                let plugin = Arc::clone(&entry.plugin);
                plugin.configure(sample_rate, max_block)?;
                plugin.set_params(params);
                return Ok(plugin);
            }
            // The device became another plugin: release the old instance.
            if let Some(old) = entries.remove(&device) {
                old.plugin.destroy();
            }
        }
        let info =
            self.inner.directory.find(kind).ok_or_else(|| {
                PluginError::Plugin(format!("the plugin {kind} is not installed"))
            })?;
        let plugin: Arc<dyn HostedPlugin> = match info.format {
            Format::Clap => ClapPlugin::create(
                Arc::clone(&self.inner.executor),
                info,
                sample_rate,
                max_block,
                state,
            )?,
            Format::Vst3 => {
                return Err(PluginError::Plugin(
                    "VST3 hosting is not available in this build".into(),
                ));
            }
        };
        plugin.set_params(params);
        entries.insert(
            device,
            Entry {
                kind: kind.to_string(),
                plugin: Arc::clone(&plugin),
            },
        );
        Ok(plugin)
    }

    /// The live instance for `device`, if any.
    pub fn get(&self, device: u64) -> Option<Arc<dyn HostedPlugin>> {
        lock(&self.inner.entries)
            .get(&device)
            .map(|entry| Arc::clone(&entry.plugin))
    }

    /// Releases every instance whose device is not in `live` (a device id with the kind it has
    /// now): removed devices and devices whose kind changed.
    pub fn retain(&self, live: &BTreeSet<(u64, String)>) {
        let mut entries = lock(&self.inner.entries);
        let gone: Vec<u64> = entries
            .iter()
            .filter(|(device, entry)| !live.contains(&(**device, entry.kind.clone())))
            .map(|(device, _)| *device)
            .collect();
        for device in gone {
            if let Some(entry) = entries.remove(&device) {
                entry.plugin.destroy();
            }
        }
    }

    /// Releases every instance (a project was opened or created: its device ids mean other
    /// devices now).
    pub fn clear(&self) {
        let drained: Vec<Entry> = lock(&self.inner.entries).drain().map(|(_, e)| e).collect();
        for entry in drained {
            entry.plugin.destroy();
        }
    }

    /// How many instances are alive.
    pub fn len(&self) -> usize {
        lock(&self.inner.entries).len()
    }

    /// True when no instance is alive.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Serves every instance's host requests and returns the devices that asked for something
    /// the app has to act on (a latency change needs a recompile).
    pub fn service(&self) -> Vec<(u64, Requests)> {
        let plugins: Vec<(u64, Arc<dyn HostedPlugin>)> = lock(&self.inner.entries)
            .iter()
            .map(|(device, entry)| (*device, Arc::clone(&entry.plugin)))
            .collect();
        plugins
            .into_iter()
            .map(|(device, plugin)| (device, plugin.service()))
            .filter(|(_, requests)| requests.any())
            .collect()
    }

    /// Each live instance's state, by device id and format; an instance that cannot save is
    /// left out. Runs the saves on the plugin main thread.
    pub fn collect_states(&self) -> Vec<(u64, Format, Vec<u8>)> {
        let plugins: Vec<(u64, Arc<dyn HostedPlugin>)> = lock(&self.inner.entries)
            .iter()
            .map(|(device, entry)| (*device, Arc::clone(&entry.plugin)))
            .collect();
        let mut states: Vec<(u64, Format, Vec<u8>)> = plugins
            .into_iter()
            .filter_map(|(device, plugin)| {
                let bytes = plugin.save_state().ok()?;
                Some((device, plugin.info().format, bytes))
            })
            .collect();
        states.sort_by_key(|(device, _, _)| *device);
        states
    }
}
