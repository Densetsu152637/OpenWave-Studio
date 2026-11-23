# Third-party plugins

OpenWave Studio hosts CLAP plugins as track instruments and insert effects, on Windows, macOS and Linux. They appear next to the built-in devices in the effect menu ("Add effect") and, for instruments, in the menu next to "Add MIDI track". A plugin's parameters show in the generic device panel, can be set by an AI agent through [MCP](mcp.md), and its state is saved with the project.

Not available yet: VST3 plugins and plugin editor windows (the plugin's own window); until they ship, plugins are edited through the generic panel.

## Finding plugins

OpenWave looks for `*.clap` files in:

| System | Folders |
| --- | --- |
| Windows | `%COMMONPROGRAMFILES%\CLAP`, `%LOCALAPPDATA%\Programs\Common\CLAP` |
| macOS | `/Library/Audio/Plug-Ins/CLAP`, `~/Library/Audio/Plug-Ins/CLAP` |
| Linux | `/usr/lib/clap`, `~/.clap` |
| Every system | the folders in the `CLAP_PATH` environment variable, and the extra folders from Settings, under Folders (absolute paths) |

The app scans when it starts. Use **Scan for plugins** in Settings, under Folders, to scan again after installing something or changing the folders (save the settings first); **Rescan all** loads every file again, also those that did not change. An agent uses the `scan_plugins` tool.

### Why scanning uses a second process

Reading a plugin's parameters means running its code. Each plugin file is therefore loaded in a separate process (the app starts itself as `openwave-studio --scan-plugin <file>`), with a 10 second limit. A plugin that crashes or hangs is skipped, listed under "Skipped" in the settings (and in the `failures` of `scan_plugins`), and not tried again until the file changes or you choose **Rescan all**. The results are kept in `plugin-cache.json` in the app's plugin cache folder (see [portable.md](portable.md) for where that is), keyed by file path, size and modification time.

Plugins run inside the app after scanning, for speed: a plugin that crashes while playing takes the app down. Save often when you try a new plugin.

## In a project

- A hosted plugin is a device of kind `clap:<plugin id>`. A project that uses a plugin you do not have (or have not scanned) opens fine: the plugin is silent (an effect passes the sound through) and keeps its settings until a scan finds it.
- Changing a parameter does not interrupt playback or restart the plugin. Other changes to a device (bypass, removing it) rebuild the audio graph as for built-in devices.
- The plugin's reported latency is compensated like any other: tracks with and without the plugin stay aligned. A plugin that changes its latency while running triggers a recompile.
- Save, and the export and analysis renders, collect each plugin's own state (presets, anything beyond the parameters) into the project file. Saving adds no undo step. On opening the project the state is restored before playback.
- Export and analysis render with separate plugin instances, so a render never disturbs what is playing.

## Fixture plugins (for developers)

The crate `ow-plugin-fixtures` builds test plugins used by the tests and for manual checks:

```sh
cargo build -p ow-plugin-fixtures --release
```

writes `ow-fixture-gain.clap` and `ow-fixture-crash.clap` to `target/plugin-fixtures/` (other profiles, such as plain `cargo build` and `cargo test`, write to `target/plugin-fixtures-debug/`). On macOS each is a `.clap` bundle folder, elsewhere a shared library renamed `.clap`.

| Plugin | What it is |
| --- | --- |
| `com.openwave.fixture.gain` (in `ow-fixture-gain.clap`) | A stereo effect with a `Gain` parameter (id 1, 0 to 2), a `Polarity` enumeration (id 2), a fixed latency of 64 frames and a small binary state. |
| `com.openwave.fixture.tone` (same file) | An instrument with a note input and a `Level` parameter (id 1): while a note is held it outputs a constant level, which makes notes easy to measure. |
| `com.openwave.fixture.crash` (in `ow-fixture-crash.clap`) | Aborts the process when it is instantiated (with `OW_FIXTURE_CRASH_MODE=hang` it never returns), to prove that scanning survives a bad plugin. |

To try them in a built app, add `target/plugin-fixtures` as an extra plugin folder in Settings (or through MCP: `audio_settings` `set` with `{"paths": {"pluginDirs": ["<absolute path of target/plugin-fixtures>"]}}`, then `scan_plugins`). The crashing fixture shows up under "Skipped".

`cargo test -p ow-plugin-host` (scanner, parameters, state, latency, realtime safety), `cargo test -p openwave-studio plugin_state` (saving and reopening) and the `mcp` tests build the fixtures and the `ow-scan` scanner program themselves, so a fresh checkout needs no extra step. The fixtures are built by a nested `cargo rustc --crate-type cdylib` that the crate's `build.rs` runs, because Cargo cannot copy a crate's own shared library to a known folder.

## Code map

- `crates/ow-plugin-host`: the format-independent traits (`HostedPlugin`, `PluginProcessor`), the CLAP implementation on `clack-host`, the scanner and its cache, `PluginCache` and the `ow-scan` program.
- `crates/ow-engine`: `CompileContext::plugins`, the `clap:<id>` branch of the compiler and `DeviceNode::hosted_*`.
- `src-tauri/src/plugins.rs`: the main-thread executor, the registry, the `plugins_scan` and `plugins_list` commands and the background scan; `src-tauri/src/ipc/control.rs`: the parameter fast path and state embedding.
- Architecture: [architecture.md](architecture.md), section 8.
