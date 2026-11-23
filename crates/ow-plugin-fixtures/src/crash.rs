//! The plugin that crashes: its descriptor is fine, but instantiating it aborts the process (or,
//! when `OW_FIXTURE_CRASH_MODE=hang`, never returns). A scanner that instantiates plugins to
//! read their parameters must survive it by running each candidate in a child process.

use std::ffi::CStr;

use clack_plugin::clack_export_entry;
use clack_plugin::entry::prelude::*;
use clack_plugin::prelude::*;

use crate::{CRASH_ID, CRASH_MODE_ENV};

pub struct CrashEntry {
    factory: PluginFactoryWrapper<CrashFactory>,
}

impl Entry for CrashEntry {
    fn new(_bundle_path: Option<&CStr>) -> Result<Self, EntryLoadError> {
        Ok(Self {
            factory: PluginFactoryWrapper::new(CrashFactory {
                descriptor: PluginDescriptor::new(CRASH_ID, "OpenWave Fixture Crash")
                    .with_vendor("OpenWave Studio tests"),
            }),
        })
    }

    fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
        builder.register_factory(&self.factory);
    }
}

pub struct CrashFactory {
    descriptor: PluginDescriptor,
}

impl PluginFactoryImpl for CrashFactory {
    fn plugin_count(&self) -> u32 {
        1
    }

    fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
        (index == 0).then_some(&self.descriptor)
    }

    fn create_plugin<'a>(
        &'a self,
        _host_info: HostInfo<'a>,
        _plugin_id: &CStr,
    ) -> Option<PluginInstance<'a>> {
        if std::env::var(CRASH_MODE_ENV).as_deref() == Ok("hang") {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
        std::process::abort()
    }
}

clack_export_entry!(CrashEntry);
