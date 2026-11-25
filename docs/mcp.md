# OpenWave Studio and AI agents (MCP)

OpenWave Studio hosts a [Model Context Protocol](https://modelcontextprotocol.io) server, so an AI agent such as Claude Code or Claude Desktop can use the running app as an editor: read the project, add tracks, notes and clips, set levels, pans, sends and effects, mix and master with measurements, import audio, record, and export. The agent sees and changes the same project as the person in front of the window: every edit shows up in the UI at once and can be undone with Undo.

There are no generative tools. The agent writes notes and arranges clips itself; it does not ask OpenWave to compose.

## Set up

Build or install the app, then register the `mcp` subcommand of the executable with your client.

Claude Code:

```sh
claude mcp add openwave -- <path to openwave-studio> mcp
```

On Windows the path is the `.exe`, for example `claude mcp add openwave -- "C:\Program Files\OpenWave Studio\openwave-studio.exe" mcp`. A local build is at `target/debug/openwave-studio` (or `target/release/openwave-studio`) after `cargo build -p openwave-studio` or `pnpm tauri build --debug`.

Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "openwave": {
      "command": "<path to openwave-studio>",
      "args": ["mcp"]
    }
  }
}
```

`openwave-studio mcp` is a small relay between the client's stdio and the app. If OpenWave Studio is not running it starts the app (a copy of the same executable, detached, so it keeps running when the client exits) and waits up to 20 seconds for it to be ready. Only one app is started: a copy that is already running is used.

The setting **Allow AI control (MCP)** in Settings, under AI control, is on by default. Turn it off and the app stops listening; `openwave-studio mcp` then reports that AI control is off instead of starting another copy. The agent cannot change this setting.

## What the agent can do

Every tool returns structured content (JSON) and a short text summary. Argument names are `snake_case`. Results reuse the UI's own shapes, so the project (`get_project`) uses the same field names as the saved `project.json`, and the status objects use the UI's `camelCase` names.

| Tool | What it does |
| --- | --- |
| `get_project` | The whole project: settings, tempo and time-signature maps, tracks and buses with their strips (volume, pan, mute, solo, output, sends), the master, clips with MIDI notes, the media pool, devices with their parameter values, automation, markers; plus history (`can_undo`, `dirty`, armed tracks, missing media, engine error) and the saved path. |
| `get_device_types` | The built-in device kinds (synth, EQ, compressor, reverb, limiter) with their parameters (`builtin`), and the scanned third-party plugins with theirs (`plugins`, kinds `clap:<plugin id>` and `vst3:<class id>`; `editor: true` marks a plugin whose editor window this platform can show). |
| `edit` | One or more `EditCommand`s, exactly the commands the UI sends. Several commands are one transaction and one undo step; if one is refused, none is applied. Answers a summary of the model changes. |
| `alloc_ids` | Reserves entity ids. New tracks, buses, clips, devices and markers need ids that no other entity has; reserve them first. |
| `undo`, `redo` | Step through history. |
| `transport` | `play`, `stop`, `locate` (to a beat), `status` (audio state, position in frames and beats, `moving`, true while playing or counting in; `counting_in` and `recording` tell them apart). |
| `project` | `new`, `open`, `save`, `save_as` (a `.owave` bundle directory). |
| `import_audio` | Decodes an audio file into the media pool and answers the media id, plus the `tempo` and `key` detected in it (nothing for a file without a clear beat or over ten minutes); a clip that references it is added with `edit`. |
| `library_scan` | Scans the loop and sample library (the folders in the preferences, `audio_settings` `set` with `{"paths": {"libraryDirs": ["/absolute/folder"]}}`) for tempo, key and length; `force: true` analyses unchanged files again, `action: "status"` reports progress, `"cancel"` stops it. |
| `library_search` | Searches the scanned library by words, tags, tempo range (`bpm_min`, `bpm_max`, `include_half_double`) and `key` (such as `Am`); answers path, tempo, key, length and tags. |
| `library_preview` | `start` plays a library file (`path` from `library_search`) through the output, stretched to the project tempo, optionally shifted by `semitones`; `stop` ends it. |
| `record` | `arm` or disarm an audio or MIDI track, `start` and `stop` recording (`midi_mode` on `start` for MIDI tracks). The takes become clips when recording stops. |
| `midi_edit` | Quantize, legato, humanize, transpose, scale or set velocity on the notes of a MIDI clip (or on chosen notes), computed by OpenWave, as one undo step. |
| `import_midi` | Reads a `.mid` file (format 0 or 1) into MIDI clips, on new MIDI tracks with a synth or on one existing MIDI track. |
| `export_midi` | Writes MIDI clips, the clips of tracks or every MIDI track to a `.mid` file. |
| `analyze` | Renders the project (the master, or one strip after its fader; whole project or a beat range) and measures integrated, short-term and momentary loudness, true peak, sample peak, RMS, crest factor, stereo correlation and eight band energies. |
| `export_wav` | Renders to a 32-bit float WAV, optionally normalised to a target integrated loudness with a true-peak ceiling. |
| `audio_settings` | List audio devices and MIDI inputs, get and set preferences, retry a failed device, toggle the test tone. |
| `scan_plugins` | Scans for CLAP and VST3 plugins: the standard folders of the OS, `CLAP_PATH`, `VST3_PATH` and the extra folders in the preferences (`audio_settings` `set` with `{"paths": {"pluginDirs": ["/absolute/folder"]}}`). Each file is loaded in a child process, so a plugin that crashes or hangs is skipped and reported. `force: true` scans unchanged files again. |
| `list_plugins` | The plugins the last scan found and the files it skipped, without scanning again. |
| `plugin_editor` | `open` or `close` the editor window of a hosted plugin (`device`: the id of a plugin device in the project). Opening answers the window's size; opening an open editor brings it forward; closing a closed one is harmless. Changes made in the window arrive as `set_device_param` edits. Windows and macOS only: on Linux the answer is `{"available": false}` with the reason; a device that is not a loaded plugin, or a plugin without an editor, is an error. |

The schema of every tool, including the full shape of each edit command, comes with `tools/list`.

### Recording

`record` records audio from the open input device (`audio_settings` lists devices and chooses one). A track must be an audio track and armed (`record` `arm`); `start` records on every armed track from the current position and `stop` ends it, after which each take is a clip on its track (one undo step, "Record"). How a recording runs comes from the project, which `get_project` shows and `edit` changes:

- **Input.** `set_track_input` with `track` and `input` sets the channels a track records: `{"channels": {"mono": 2}}` records input channel 2 (a mono take), `{"channels": {"stereo": 0}}` records the pair 0 and 1; `null` is the default (channel 0 on a one-channel input, channels 0 and 1 on a wider one). Channels count from 0 on the open device; starting a recording with a track routed to channels the device does not have is an error that names the track.
- **Monitoring.** `set_track_monitor` with `track` and `monitor`: `off`, `auto` (the input is heard while the track is armed) or `on`. The input is played through the track's strip before its inserts, so effects, fader and pan apply. It needs an open input device; changing the mode recompiles the audio graph, arming does not.
- **Punch.** `set_settings` with `punch_range` (`{"start": 8, "end": 16}`, in beats) and `punch_enabled: true`: only the part of the take inside the range is recorded, and the take starts at punch-in.
- **Count-in.** `count_in_bars` (0 to 16) in the same settings: the metronome counts in, the transport reports `counting_in`, and nothing is recorded until the start position.
- **Loop takes.** With `loop_range` set, the transport loops while recording and every pass of the loop is a take of its own (up to 16 per recording; later passes are not recorded and `stop` reports it). The takes of one recording on a track are clips that share `take.group`, with `take.index` counting the passes from 0. Exactly one take of a group is active and plays: the last pass at first. `set_active_take` with `track`, `group` and `index` plays another one and silences the rest (one undo step). Takes are whole clips, so a take is a recording of the loop region and can be moved, trimmed, split or deleted like any clip (a split or duplicate is no longer a take).
- **Takes are plain clips.** Mute and clip gain belong to the clip they are set on, so switching the active take can change what is muted or how loud it plays. Moving a take (in the app) carries the whole group; split, duplicate and trim-left/right need a group with a single take. A group whose active take was deleted has no playing take until `set_active_take` chooses one.

Monitoring feeds the input to the output: when the input device hears the output (speakers, or a digital loopback), `auto` or `on` monitoring feeds back. Whether to monitor is the user's choice; wear headphones or use `off`. The number of loop takes is 16 for up to four armed tracks and fewer with more (a track keeps at least 2).

The timeline placement already compensates the device's round-trip latency, so a take lines up with what was played. Arming, like transport state, is session state: it is not saved and has no undo step.

### Loops, tempo and warp

`import_audio` and the media pool carry what the importer detected: a media item has an optional `tempo` (beats per minute) and `key` (`tonic` 0 to 11 from C, `minor`, `confidence`), both left out when nothing clear was found. An audio clip follows the project tempo when its content has a `warp`:

```json
{"type": "audio", "media": 7, "source_offset": 0, "gain_db": 0, "fade_in": 0, "fade_out": 0,
 "warp": {"mode": "follow_tempo", "source_bpm": 120, "markers": [], "transpose_semitones": 0}}
```

`mode` is `off` (the file plays as it is) or `follow_tempo`: the audio is stretched so that `source_bpm` of it fits the project tempo map, which makes the clip `length` (in beats) the number of beats the loop has at `source_bpm`: an eight-beat loop recorded at 120 bpm is a clip of `length` 8 at any project tempo. `transpose_semitones` (-24 to 24) shifts the pitch without changing the length. `markers` (`{"source_frame", "beat"}`, rising in both) pin a frame of the file to a clip-local beat and bend the stretch between them; with markers `source_offset` is not used. Set it with `edit` `replace_clip`; the stretched audio is rendered when the project compiles and cached under the project's `cache/` folder, so a tempo change takes a moment before the new timing plays. Tempo ratios beyond 4x either way play silence for that part.

Adding a loop: `library_search`, then `import_audio` with the path, then an `edit` that adds the clip with `warp` set (use the `tempo` the import answers when the file name has none). `library_preview` is only an audition; it does not touch the project.

### MIDI

`midi_edit` takes a `clip`, an `edit` and optionally `notes` (indices into the clip's `notes` array). The operations: `{"op":"quantize","grid":0.25,"strength":1,"lengths":false}` (grid in beats; strength 0 to 1 moves each start that fraction toward the nearest grid line; `lengths` also snaps lengths to whole grid steps); `{"op":"legato","mode":"any_key"}` (each note is stretched or trimmed to end where the next note starts, on any key, or `same_key`; the last note keeps its length); `{"op":"humanize","seed":7,"timing":0.02,"velocity":8}` (random but repeatable changes up to `timing` beats and `velocity` steps; the same seed and notes give the same result); `{"op":"transpose","semitones":12}` (notes that would leave 0 to 127 are removed); `{"op":"velocity_scale","factor":0.8}`; `{"op":"velocity_set","velocity":90}`. The answer gives the note counts before and after. An agent can also write the notes itself with `edit` (`replace_clip`).

`import_midi` reads a Standard MIDI File (format 0 or 1; at most 32 MiB; SMPTE timing and format 2 are refused). Each file track that holds notes or control changes becomes one clip, starting where the music starts (rounded down to a beat) plus `start_beat`, on a new MIDI track with a synth, or all on the MIDI track `track`. The file's tempo changes replace the project's tempo map only with `apply_tempo: true`. One beat is one quarter note. `export_midi` writes a format 1 file at 960 pulses per quarter note: a tempo track (the project's tempo points, a ramp as a step; its first time signature), then one track per MIDI track at the clips' positions, each note cut at its clip's end as it plays. Paths must end in `.mid` or `.midi`, and an existing file is replaced only with `overwrite: true`.

#### Recording MIDI

`record` `arm` takes a MIDI track too. An armed MIDI track plays what comes in on the connected MIDI inputs (`audio_settings` lists and chooses them; inputs marked as control surfaces are not note input) through its instrument, with the transport stopped or playing, and `start` records those messages on every armed MIDI track, with no audio input device needed. Stopping adds the clips in the same one "Record" undo step as audio takes. Count-in, punch and loop come from the project as for audio. `midi_mode` on `start` decides what happens to the MIDI clips already on the track: `replace` (the default: the recording is a new clip and replaces the MIDI clips under its range, cutting clips that straddle it; with a loop every pass is a clip of its own over the loop, the last one playing and the earlier ones muted: unmute the one you prefer and mute the others), `overdub` (the recorded notes join the clip they fall in, every pass of a loop adds to it, and notes outside any clip make a new clip), or `loop-merge` (all passes of a loop are merged into one clip that replaces the loop range; without a loop it is `replace`). The recorded position is the moment the message arrived, with no latency compensation.

### Third-party plugins

A scanned plugin is a device like any other (see [plugins.md](plugins.md)): `insert_device` with `kind` set to the plugin's `kind` (`clap:com.vendor.plugin`, or `vst3:` and the class id for VST3) adds it as an insert, or as a MIDI track's instrument when the plugin is an instrument; `set_device_param` sets a parameter by the plugin's own parameter id (the ids and ranges come with `get_device_types`). A parameter edit reaches the running plugin without rebuilding the audio graph. The plugin's state is saved in the project file with `project` `save` and restored when it is opened.

A plugin that is not installed (or not scanned yet) stays in the project and stays silent (an effect passes the sound through) until a scan finds it.

#### The plugin's own window

`plugin_editor` with `action: "open"` shows the plugin's editor window on the machine (the person at the computer sees it), and `action: "close"` closes it. Everything the user does in that window is an ordinary edit: a gesture on a control (from taking hold to letting go) arrives as `set_device_param` edits inside one undo step named after the parameter (for example "Edit Gain"), so `get_project` shows the new values and `undo` takes the whole gesture back. The agent can therefore read what a person did in the editor by calling `get_project` again, but it cannot click inside the window. Removing the device or closing the project closes the window.

### A typical mix-and-master loop

1. `get_project` to see the tracks and their ids.
2. `edit` with `set_strip_settings` (volume, pan, mute, solo and output of a track, a bus or the master), `set_sends` and `insert_device` for an EQ, compressor, reverb or limiter on a track, a bus or the master, and `set_device_param` for their parameters.
3. `analyze` the master, compare the integrated loudness and true peak with the target, adjust, and repeat.
4. `export_wav` with `normalize` (for example `target_lufs` -14 and `true_peak_ceiling_db` -1); its answer includes the measurements of the file it wrote.

## Safety and file access

- The agent has your file access for imports, saves and exports. `import_audio`, `project` (open and save) and `export_wav` read and write any path your user account can. Treat an agent with this server like a program you run.
- `project` with `new` or `open` refuses to throw away unsaved changes unless `discard_changes` is `true`. `export_wav` and `project` `save_as` refuse to replace an existing file or bundle unless `overwrite` is `true`. These checks stop accidents. They are not a defence against a prompt-injected or malicious agent, which can still overwrite any `.wav` file or any OpenWave project bundle you can write, import any audio file you can read, and read anything saved into a project.
- Hard limits that `overwrite` does not lift: `export_wav` only writes a path that ends in `.wav` (any case), and `export_midi` only a path that ends in `.mid` or `.midi`, so they cannot replace a document or any other file type; `save_as` appends `.owave` to a path that lacks it and refuses an existing file or folder that is not an OpenWave project bundle (a folder without `project.json`).
- There is no network listener. The app listens on a local socket that only your user can open: on Windows a named pipe whose access list names only your user and that refuses remote clients, on macOS and Linux a Unix socket with mode 0600 in a directory with mode 0700 (`<app data dir>/mcp/`). The pipe name carries a random part chosen at each start.
- Every connection must present a random 256-bit session token first. The token lives in `<app data dir>/mcp/mcp.json`, which only your user can read, and changes every time the app starts. A connection with a missing or wrong token is refused and logged to the app's standard error.
- Playback is audible. `transport` `play` and the test tone sound on the audio device the app uses.

## Parity rule for developers

An agent must be able to do anything a user can do in OpenWave, except generate music. Three tests (`cargo test -p openwave-studio mcp_parity`) enforce it:

- Every Tauri command registered in `src-tauri/src/lib.rs` is listed in `COMMANDS` there (the test checks the list against `generate_handler!` and against every `#[tauri::command]` in the crate) and has a tool route in `mcp::ROUTES`, or an entry in the test's exclusion list with a reason. The exclusions are the updater, the telemetry stream, the patch-channel attach and the waveform bytes. The plugin commands (`plugins_scan`, `plugins_list`, `plugin_editor_open`, `plugin_editor_close`) route to `scan_plugins`, `list_plugins` and `plugin_editor`.
- Every `UiCommand` variant has a route in `mcp::UI_ROUTES`.
- The `edit` tool's schema lists every `EditCommand` variant, so a new edit command is reachable (and described) without further work.

A slice that adds a UI feature therefore adds its MCP tool, or a field on an existing tool, in the same slice: add the command to `COMMANDS`, add the route, implement it in `AppBackend::call` over the same plain `IpcState` or `EngineState` method the Tauri command wraps, and add the case to the `mcp` tests. Tool argument structs live in `crates/ow-mcp/src/tools.rs`; the catalog and the `ToolCall` enum are generated from one table there.

## Troubleshooting

- "OpenWave Studio did not open its MCP socket in time": the app failed to start or crashed during start. Start it by hand and read its standard error.
- "AI control turned off": enable **Allow AI control (MCP)** in Settings.
- A running copy of the app from before an update keeps its old tool list until restarted.
