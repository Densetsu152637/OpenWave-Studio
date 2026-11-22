# Audio and MIDI devices

How OpenWave Studio talks to audio and MIDI hardware, what the preferences store, how it recovers when a device disappears, and how to build with ASIO. Design rationale is in the [architecture record](architecture.md#5-audio-and-midi-io).

## Audio systems

The audio system is chosen in Settings › Audio. Each one is a backend in the `ow-io` crate behind one `AudioBackend` trait.

| Platform | Audio systems | Notes |
| --- | --- | --- |
| Windows | WASAPI (shared), WASAPI (exclusive, low latency) | Shared mode uses the buffer size Windows picks for the device (typically 10 ms) and cannot be changed from the app. Exclusive mode opens the device directly with a buffer you choose; no other application can use the device while it is open, and Windows must allow exclusive mode for the device (Sound settings › device properties › Advanced). Exclusive mode is output-only for now. |
| macOS | CoreAudio | Recording needs the microphone permission (see [releasing](releasing.md)). |
| Linux | ALSA | JACK and PipeWire hosts are optional cargo features, `jack` and `pipewire` on `ow-io`, because they need extra system libraries; they are not in official binaries yet. |

Exclusive mode tries a list of formats in this order and reports the one it opened in the device name: 32-bit float, 32-bit integer, 24-bit (in a 32-bit container), packed 24-bit, 16-bit, each at the requested (or the device's mix-format, 48 kHz, 44.1 kHz, 96 kHz) rate and two channels. Many USB headsets accept only 16-bit.

## Preferences

Settings are stored in `preferences.json` in the config folder (the portable data folder in portable mode, otherwise the platform config directory). The file is versioned (`version`), validated on load and on every change, and written atomically. A file that cannot be used is moved to `preferences.json.corrupt` and defaults are used; a file written by a newer build is moved to `preferences.json.newer`, and the settings dialog shows a warning with the new name.

| Section | Keys |
| --- | --- |
| `audio` | `backend` (empty: platform default), `outputDevice`, `inputDevice` (`null`: none, `"default"`: system default), `sampleRate`, `bufferFrames` (16 to 4096); `null` means the device default |
| `midi.inputs[]` | `port`, `name`, `enabled`, `controlSurface` |
| `paths` | `projectsDir`, `pluginDirs[]` (absolute paths) |
| `appearance` | `theme`: `system`, `light` or `dark` |
| `shortcuts` | action id to accelerator, only for actions that differ from the default; empty text unbinds |

Changing the audio section closes the stream and opens the new one; audio resumes in well under a second. If the new device cannot be opened, the previous one keeps running, the error is shown, and only the audio part of the change is discarded.

## Device loss and recovery

The engine watches the stream. When the device is unplugged, disabled, or its format changes, the stream is closed and reopened: first the chosen device (two attempts), then the system default device on the same audio system, up to six attempts with delays growing from 250 ms to 4 s. Settings and the status bar show "Reconnecting the audio device (attempt n of 6)". If every attempt fails the state becomes "Audio stopped" with the last error and a Retry button. The test tone and other engine state start again from scratch on the new stream, and MIDI inputs reconnect.

## MIDI

Input and output ports come from the platform MIDI layer (`midir`). Each enabled input runs on its own thread and pushes into its own wait-free ring to the audio thread; inputs marked "Control surface" share one extra ring. Messages are three bytes at most (SysEx is dropped and counted). Every message carries a monotonic microsecond timestamp taken when it arrived. Today messages are applied at the start of the next audio cycle; sample-accurate placement inside the buffer arrives with MIDI recording.

## Testing without hardware

CI runners have no audio devices, so tests use the mock backend (`ow_io::mock`). It drives the engine callback from a thread or synchronously with a fixed buffer size, injects input audio and timestamped MIDI, loops the output back to the input after a set latency, captures the output, and can simulate disconnection. Run `cargo test -p ow-io mock` for the realtime-path tests (10 000 callbacks at 64 frames under `assert_no_alloc`, MIDI from two producer threads without loss).

`cargo run -p ow-io --example device_switch -- --exclusive` is a manual check against real hardware: it lists devices, plays a quiet tone, switches buffer size and audio system while playing and prints how long audio took to resume.

## Building with ASIO (source builds only)

ASIO is not in official binaries. The ASIO SDK is available under the GPLv3 or Steinberg's proprietary license; linking it puts the resulting binary under GPLv3 obligations (see the [architecture record](architecture.md#5-audio-and-midi-io) for the decision). To build an ASIO-enabled binary yourself:

1. Windows only, with the MSVC toolchain and LLVM installed (the ASIO bindings are generated with `bindgen`; set `LIBCLANG_PATH` to the folder containing `libclang.dll`, for example `C:\Program Files\LLVM\bin`).
2. Build with the feature: `cargo build -p ow-io --features asio` (or add `--features ow-io/asio` when building the app).
3. cpal's build script downloads the SDK from Steinberg automatically. To use a copy you already have, point `CPAL_ASIO_DIR` at the unpacked SDK folder.

A binary built this way is yours to use under GPLv3; do not redistribute it under the MIT license. CI builds `ow-io` with the feature on the Windows runner (build only, nothing is published) so the feature keeps compiling; the default build contains no ASIO code, and `cargo tree -p ow-io -e features` shows no `asio`.

## Notices

`cpal` (Apache-2.0), `midir` (MIT), `wasapi` (MIT) and `rtrb` (MIT or Apache-2.0) are dependencies of `ow-io`; `cargo deny check licenses` guards the license policy for every platform.
