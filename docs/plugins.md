# Third-party plugins

OpenWave Studio hosts CLAP and VST3 plugins as track instruments and insert effects, on Windows, macOS and Linux. They appear next to the built-in devices in the effect menu ("Add effect") and, for instruments, in the menu next to "Add MIDI track". A plugin's parameters show in the generic device panel, can be set by an AI agent through [MCP](mcp.md), and its state is saved with the project. On Windows and macOS a plugin's own editor window opens from the device panel (see [Editor windows](#editor-windows)).

Not supported: AU and LV2 plugins, VST2 plugins, and editor windows on Linux (the generic panel and MCP set the same parameters there). Plugins that need more than a stereo pair on their main bus use only the first two channels; a mono bus is summed or doubled.

## Finding plugins

OpenWave looks for `*.clap` and `*.vst3` files (a `.vst3` is a single file or a bundle folder) in:

| System | Folders |
| --- | --- |
| Windows | `%COMMONPROGRAMFILES%\CLAP`, `%LOCALAPPDATA%\Programs\Common\CLAP`, `%COMMONPROGRAMFILES%\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3` |
| macOS | `/Library/Audio/Plug-Ins/CLAP`, `~/Library/Audio/Plug-Ins/CLAP`, `/Library/Audio/Plug-Ins/VST3`, `~/Library/Audio/Plug-Ins/VST3` |
| Linux | `/usr/lib/clap`, `~/.clap`, `/usr/lib/vst3`, `/usr/local/lib/vst3`, `~/.vst3` |
| Every system | the folders in the `CLAP_PATH` and `VST3_PATH` environment variables, and the extra folders from Settings, under Folders (absolute paths) |

A VST3 file can hold several plugins; each audio class in it is listed on its own.

The app scans when it starts. Use **Scan for plugins** in Settings, under Folders, to scan again after installing something or changing the folders (save the settings first); **Rescan all** loads every file again, also those that did not change. An agent uses the `scan_plugins` tool.

### Why scanning uses a second process

Reading a plugin's parameters means running its code. Each plugin file is therefore loaded in a separate process (the app starts itself as `openwave-studio --scan-plugin <file>`), with a 10 second limit. A plugin that crashes or hangs is skipped, listed under "Skipped" in the settings (and in the `failures` of `scan_plugins`), and not tried again until the file changes or you choose **Rescan all**. The results are kept in `plugin-cache.json` in the app's plugin cache folder (see [portable.md](portable.md) for where that is), keyed by file path, size and modification time.

Plugins run inside the app after scanning, for speed: a plugin that crashes while playing takes the app down. Save often when you try a new plugin.

## In a project

- A hosted plugin is a device of kind `clap:<plugin id>` or `vst3:<class id>`. A VST3 class id is 32 hexadecimal digits, the same text on every system, so a project made on one system finds its plugins on another where they are installed. A project that uses a plugin you do not have (or have not scanned) opens fine: the plugin is silent (an effect passes the sound through) and keeps its settings until a scan finds it.
- Changing a parameter does not interrupt playback or restart the plugin. Other changes to a device (bypass, removing it) rebuild the audio graph as for built-in devices.
- The plugin's reported latency is compensated like any other: tracks with and without the plugin stay aligned. A plugin that changes its latency while running triggers a recompile.
- Save, and the export and analysis renders, collect each plugin's own state (presets, anything beyond the parameters) into the project file, tagged `clap` or `vst3`. Saving adds no undo step. On opening the project the state is restored before playback. A VST3 plugin's state holds both its component and its controller state.
- Export and analysis render with separate plugin instances, so a render never disturbs what is playing.
- A VST3 plugin's parameters are converted between the plain values the project stores (the ones the generic panel and MCP show) and the normalised 0 to 1 values VST3 uses by the plugin's own controller. Note events reach VST3 instruments as note-on and note-off events on their first event bus.

## Editor windows

On Windows and macOS a plugin that has an editor gets an **Open editor** button in the device panel (select the plugin in the track's device chain). OpenWave creates a plain window and the plugin draws into it: the editor of a CLAP plugin is embedded through the CLAP `gui` extension, that of a VST3 plugin through `IPlugView`. The window follows the plugin's size and its resize requests, and the user can resize it when the plugin allows. Closing the window, removing the device or closing the project closes the editor (and so does the **plugin_editor** MCP tool, see [mcp.md](mcp.md)).

- A parameter changed in the editor is a normal edit: it shows in the device panel, in `get_project`, and an agent sees it. One gesture (from taking hold of a control to letting go) is one undo step named after the parameter (for example "Edit Gain"); a change the plugin makes without a gesture is a step of its own. Undo moves the plugin's control back too.
- Editor windows exist on Windows and macOS only. On Linux the button is hidden and `plugin_editor` answers that the editor is unavailable.
- Open editor windows are served about ten times a second (closing and resizing by the user); a plugin that animates its own window draws as it likes.

## Fixture plugins (for developers)

The crate `ow-plugin-fixtures` builds test plugins used by the tests and for manual checks:

```sh
cargo build -p ow-plugin-fixtures --release
```

writes `ow-fixture-gain.clap`, `ow-fixture-crash.clap` and `ow-fixture-gain.vst3` to `target/plugin-fixtures/` (other profiles, such as plain `cargo build` and `cargo test`, write to `target/plugin-fixtures-debug/`). On macOS each is a bundle folder (`Contents/MacOS/<name>` and an `Info.plist`); elsewhere a CLAP is a shared library renamed `.clap`, and the VST3 is a shared library renamed `.vst3` on Windows and a bundle folder (`Contents/<arch>-linux/<name>.so`) on Linux.

| Plugin | What it is |
| --- | --- |
| `com.openwave.fixture.gain` (in `ow-fixture-gain.clap`) | A stereo effect with a `Gain` parameter (id 1, 0 to 2), a `Polarity` enumeration (id 2), a fixed latency of 64 frames and a small binary state. |
| `com.openwave.fixture.tone` (same file) | An instrument with a note input and a `Level` parameter (id 1): while a note is held it outputs a constant level, which makes notes easy to measure. |
| VST3 gain, class id `4F574658565354334741494E50524F43` (in `ow-fixture-gain.vst3`) | The same effect as the CLAP gain, written against the `vst3` crate with a separate controller class: `Gain` (id 1, plain 0 to 2), `Polarity` (id 2, two steps), 64 frames of latency, component and controller state, and an editor view. |
| VST3 tone, class id `4F57465856535433544F4E4550524F43` (same file) | The tone instrument as one object that is component, processor and controller at once (the single-component layout), with an event input and a `Level` parameter (id 1). |
| `com.openwave.fixture.crash` (in `ow-fixture-crash.clap`) | Aborts the process when it is instantiated (with `OW_FIXTURE_CRASH_MODE=hang` it never returns), to prove that scanning survives a bad plugin. |

To try them in a built app, add `target/plugin-fixtures` as an extra plugin folder in Settings (or through MCP: `audio_settings` `set` with `{"paths": {"pluginDirs": ["<absolute path of target/plugin-fixtures>"]}}`, then `scan_plugins`). The crashing fixture shows up under "Skipped".

The CLAP gain and the VST3 gain each have a minimal editor: a plain native child view (Win32 child window or AppKit subview) that fills the host's window. The fixture libraries also export three functions for tests (`ow_fixture_simulate_edit`, `ow_fixture_request_resize`, `ow_fixture_editor_state`, see `ow_plugin_fixtures::hooks`) that stand in for the editor's user: they report a parameter gesture, ask the host for a new window size and tell whether the view is attached. The hooks act on the plugin instance made last, so tests that use them run one at a time.

`cargo test -p ow-plugin-host` (scanner, parameters, state, latency, realtime safety, VST3, and `--test editor` for the windows), `cargo test -p openwave-studio plugin_state` (saving and reopening, editor gestures as undo steps) and the `mcp` tests build the fixtures and the `ow-scan` scanner program themselves, so a fresh checkout needs no extra step. The editor test has its own `main`: AppKit creates windows on the process's main thread only, so that thread plays the app's event loop while the checks run on another. The fixtures are built by a nested `cargo rustc --crate-type cdylib` that the crate's `build.rs` runs (once per library), because Cargo cannot copy a crate's own shared library to a known folder.

## Code map

- `crates/ow-plugin-host`: the format-independent traits (`HostedPlugin`, `PluginProcessor`), the CLAP implementation on `clack-host` (`clap.rs`), the VST3 implementation on the `vst3` crate's bindings (`vst3/`: `module.rs` loads the library and its factory, `host.rs` has the host objects handed to plugins, `audio.rs` the audio side, `view.rs` the editor view and frame, `plugin.rs` the instance and the scan), the editor windows (`editor/`: `win32.rs`, `macos.rs`), the scanner and its cache, `PluginCache` and the `ow-scan` program.
- `crates/ow-engine`: `CompileContext::plugins`, the `clap:<id>` and `vst3:<id>` branch of the compiler and `DeviceNode::hosted_*`.
- `src-tauri/src/plugins.rs`: the main-thread executor, the registry, the `plugins_scan`, `plugins_list`, `plugin_editor_open` and `plugin_editor_close` commands, the background scan and the thread that applies editor changes to the project; `src-tauri/src/ipc/control.rs`: the parameter fast path and state embedding.
- Architecture: [architecture.md](architecture.md), section 8.
