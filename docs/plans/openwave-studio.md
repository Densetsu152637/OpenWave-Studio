# Plan: OpenWave Studio

Revision: 2026-11-22 (initial plan; no stage started)

Goals: each is true when its verifying command or observation succeeds.

- G1. The app builds and launches on Windows, macOS and Linux: `gh run list --workflow ci.yml --branch main --limit 1 --json conclusion` reports `success`, and a downloaded release bundle opens its main window on each OS.
- G2. Pushing a `v*` tag publishes a stable release and the `main` feed; pushing to `experimental` publishes an experimental pre-release and the `experimental` feed: `gh release view updater-main` and `gh release view updater-experimental` each list `latest.json`.
- G3. An installed or portable build on either channel updates itself to the next build of the same channel: observed by installing build N, publishing N+1, and seeing the app report and apply N+1.
- G4. A user can record audio and MIDI, arrange, mix, automate and export a song using only built-in instruments and effects: the end-to-end fixture `crates/ow-engine/tests/fixtures/demo_song.owave` renders to WAV in `cargo test -p ow-engine --test render` and the manual journey in stage 22 succeeds.
- G5. Third-party CLAP, VST3, AU (macOS) and LV2 (Linux) plugins load, play and save state: plugin-host integration tests pass in CI on all three OSes.
- G6. OpenWave's built-in devices load in other DAWs as CLAP and VST3 (and AUv2 on macOS): `clap-validator` and the VST3 `validator` pass in CI and the plugin bundle zip is attached to releases.
- G7. Official binaries stay MIT-compatible: `cargo deny check licenses` passes in CI with GPL/LGPL/AGPL denied.

Scope / non-goals: scope is the application, its CI and release pipeline, the updater, and plugin hosting and export as described in the [architecture record](../architecture.md). Non-goals: VST2, AAX, mobile, cloud or collaboration, video tracks, notation, MP3 export, ASIO in official binaries, bundling the WebView2 runtime.

Assumptions:

- Repository `Densetsu152637/OpenWave-Studio` on GitHub with Actions enabled, branches `main` and `experimental`; work lands on `experimental` and `main` only takes merges from `experimental` (enforced from stage 2).
- Repository secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` are created by the owner in stage 3; Apple signing secrets are optional (builds proceed unsigned without them, as in Imperium-AI).
- Toolchains: stable Rust, Node 24 with pnpm, Tauri CLI 2.x; current versions at plan time are listed in the architecture record's sources.
- Builds and test suites run in CI (CI-first); implementers run only focused local checks.

Relevant paths: `Cargo.toml`, `package.json`, `src/`, `src-tauri/`, `crates/*`, `plugins/*`, `xtask/`, `scripts/`, `.github/workflows/`, `docs/`. Reference CI: `D:\Git Repositories\Imperium-AI\.github\workflows\` and `D:\Git Repositories\Imperium-AI\scripts\ci\`.

Related docs: [docs/architecture.md](../architecture.md) wins on design decisions (crates, formats, licensing, IPC, threading); this plan wins on stage order, ownership and acceptance.

## How release CI mirrors Imperium-AI

| Imperium-AI | OpenWave Studio |
| --- | --- |
| `ci.yml`: PRs, pushes to `main`/`experimental`, merge queue; detect, docs, actionlint, rust, web, `source branch` (PRs into `main` only from `experimental`), `CI gate` | same jobs, plus `cargo deny`, and from stage 23 plugin validators |
| `build.yml` reusable (`channel`, `version`), five-target matrix, `scripts/ci/build-desktop.sh` calling `tauri build` | same matrix and inputs; `tauri-apps/tauri-action@v1` in build-only mode (no `tagName`/`releaseId`) with `--target`, `--bundles`, `--config {version, bundle.createUpdaterArtifacts}`; `scripts/ci/collect-dist.sh` copies `artifactPaths` and builds portable zips into `dist/` |
| `release.yml`: tags `v*`, `!v*-experimental*`; `release-prepare.sh` (hyphen = pre-release); `publish-release.sh` (`--latest`, `SHA256SUMS`, prune) | identical trigger and prepare logic; publish also uploads `latest.json` to the fixed `updater-main` release, only for non-pre-releases |
| `experimental.yml`: push to `experimental`; version `X.Y.<run>-experimental`; pre-release `--latest=false`; prune to one | identical; publish uploads `latest.json` to the fixed `updater-experimental` release |
| updater feed = `latest.json` asset of the newest channel release, found at runtime via the GitHub REST API | feed = fixed URL `releases/download/updater-<channel>/latest.json` (no API rate limit, no client listing logic) |
| `updater-manifest.sh` maps signed installers to platform keys | same, plus `windows-<arch>-portable` keys pointing at signed portable zips |
| MSI/RPM skipped for hyphenated versions; empty signing secrets unset | same |

## Stage 1: Base app scaffold and engine smoke path

- Objective: a Tauri v2 app (Svelte 5 + TypeScript + Vite, pnpm) with the workspace layout from the architecture record, a minimal UI shell (menu bar, empty arrangement area, transport bar with play/stop, status bar showing audio device and CPU), a minimal `ow-engine` that opens the default output device through `cpal` and plays a 440 Hz test tone when "Play test tone" is pressed, and the portable-mode `paths` module (marker detection, data directory, WebView2 data directory). Local check scripts `scripts/check-rust.sh` (fmt, clippy `-D warnings`, test) and `scripts/check-web.sh` (lint, svelte-check, vitest, build).
- Depends on: none
- Owned paths: `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `package.json`, `pnpm-lock.yaml`, `pnpm-workspace.yaml`, `vite.config.ts`, `svelte.config.js`, `tsconfig.json`, `index.html`, `src/`, `src-tauri/`, `crates/ow-engine/`, `crates/ow-io/`, `scripts/check-rust.sh`, `scripts/check-web.sh`, `README.md` (build section), `.gitignore`
- Acceptance checks:
  - Rust checks pass on all three OSes: true when `bash scripts/check-rust.sh` exits 0 on Windows, macOS and Linux (verified by stage 2's CI run; before then, on the implementer's OS plus `cargo check --target` where available).
  - Web checks pass: `bash scripts/check-web.sh` exits 0.
  - App launches and the tone plays: `pnpm tauri dev` opens the window, pressing "Play test tone" produces audio and the status bar shows the device name (manual observation, recorded in Outcome).
  - Portable mode: with a `portable` file beside the built executable, settings are written under `OpenWave Studio Data/` beside it; `cargo test -p openwave-studio paths` exits 0.
- Status: pending
- Outcome:
- Handoff notes: keep the engine smoke path behind the same `Engine` handle later stages extend (do not build a throwaway). Enable `tauri-plugin-updater` only in stage 3. App identifier `org.openwave.studio`, product name `OpenWave Studio`.

## Stage 2: Release CI

- Objective: CI and release workflows mirroring Imperium-AI as summarized above: `ci.yml`, `build.yml`, `release.yml`, `experimental.yml`, `scripts/ci/*` (`detect.sh`, `install-linux-deps.sh`, `release-prepare.sh`, `experimental-prepare.sh`, `collect-dist.sh`, `publish-release.sh`, `publish-experimental.sh`, `prune-releases.sh`, `check-links.py`), `.markdownlint-cli2.jsonc`, `deny.toml`. Create the `experimental` branch. Portable artifacts: Windows plain-exe zip with `portable` marker, macOS `.app` zip with marker, Linux AppImage. Pin actions by SHA as Imperium does.
- Depends on: stage 1
- Owned paths: `.github/workflows/`, `scripts/ci/`, `deny.toml`, `.markdownlint-cli2.jsonc`, `docs/releasing.md`
- Acceptance checks:
  - CI green on a PR: `gh pr checks <pr>` shows `CI gate` passing with rust jobs on Windows, macOS and Linux.
  - Source-branch rule: a PR from a feature branch into `main` fails `source branch` (observed once, then closed).
  - Experimental release: a push to `experimental` produces a pre-release `vX.Y.N-experimental` with Windows, macOS and Linux installers, portable zips and `SHA256SUMS`: `gh release view vX.Y.N-experimental --json assets`.
  - Stable release: pushing tag `v0.1.0` produces a release marked latest with the same asset set: `gh release view v0.1.0`.
  - `actionlint` and `cargo deny check licenses` pass in CI.
- Status: pending
- Outcome:
- Handoff notes: until stage 3 adds signing, `createUpdaterArtifacts` is false and no `latest.json` is written (the manifest script warns and exits 0, as in Imperium). `docs/releasing.md` documents tag format, channels and required secrets.

## Stage 3: Auto-updater with main and experimental channels

- Objective: `tauri-plugin-updater` with the public key in `tauri.conf.json`; signing secrets wired into `build.yml`; `scripts/ci/updater-manifest.sh` writing `latest.json` (installer and portable keys); publish scripts uploading it with `--clobber` to the fixed `updater-main` and `updater-experimental` releases; `src-tauri/src/update.rs` implementing channel selection (default from version, switchable in settings), scheduled checks (20 s after start, every 6 h), background download, install on quit or "restart now", downgrade-allowed comparator after a channel switch, and the Windows portable self-update path (`--apply-update` helper). UI: update status in settings and a non-modal "update ready" notice.
- Depends on: stage 2
- Owned paths: `src-tauri/src/update.rs`, `src-tauri/src/portable_update.rs`, `src-tauri/tauri.conf.json` (updater section), `src-tauri/capabilities/`, `src/lib/update/`, `src/routes/settings/` (update panel), `scripts/ci/updater-manifest.sh`, `scripts/ci/publish-*.sh`, `.github/workflows/build.yml` (secrets), `docs/releasing.md`
- Acceptance checks:
  - Feeds exist: `gh release view updater-main --json assets` and `gh release view updater-experimental --json assets` list `latest.json`; `curl -sL https://github.com/Densetsu152637/OpenWave-Studio/releases/download/updater-experimental/latest.json | jq .platforms` lists `windows-x86_64`, `windows-x86_64-portable`, `darwin-aarch64`, `darwin-x86_64`, `linux-x86_64`.
  - Experimental installed update: install experimental build N, publish N+1, the app reports and installs N+1 (observed on Windows and one other OS).
  - Portable update: Windows portable build N updates to N+1 in place and keeps its `OpenWave Studio Data/` folder (observed).
  - Channel switch: switching an experimental install to `main` installs the current stable release (observed).
  - Unit tests for channel derivation and feed URLs pass: `cargo test -p openwave-studio update` (in CI).
- Status: pending
- Outcome:
- Handoff notes: the owner generates the key pair with `pnpm tauri signer generate` and adds both secrets; the agent must not handle the private key. Create the two `updater-*` releases once (pre-release, `--latest=false`) from the publish script if absent.

## Stage 4: Engine graph core (single-threaded)

- Objective: `ow-engine` audio graph: `Node` trait, buffer pool, `RenderPlan`, plan swap over `rtrb` with old-plan return, parameter messages, offline render driver, and the real-time safety harness (`assert_no_alloc` around the cycle in debug and test builds).
- Depends on: stage 1
- Owned paths: `crates/ow-engine/src/graph/`, `crates/ow-engine/src/plan.rs`, `crates/ow-engine/src/offline.rs`, `crates/ow-engine/tests/`
- Acceptance checks:
  - Offline render of a fixture graph (oscillator into gain into master) matches the reference within -90 dBFS: `cargo test -p ow-engine --test render` (CI).
  - No allocation in the cycle, including across 1000 plan swaps: `cargo test -p ow-engine --test rt_safety` (CI).
- Status: pending
- Outcome:
- Handoff notes: interfaces defined here (`Node`, `ProcessContext`, `RenderPlan`) are consumed by every later engine stage; document them with rustdoc.

## Stage 5: Multithreaded scheduler and delay compensation

- Objective: DAG scheduler with per-node atomic dependency counters, lock-free ready queue, real-time worker pool (`audio_thread_priority`), user-set worker count, plugin delay compensation in plan compilation, and `criterion` benchmarks including the track-count benchmark.
- Depends on: stage 4
- Owned paths: `crates/ow-engine/src/sched/`, `crates/ow-engine/src/pdc.rs`, `crates/ow-engine/benches/`
- Acceptance checks:
  - Renders are bit-identical for 1, 2 and 8 workers: `cargo test -p ow-engine --test determinism`.
  - PDC aligns a 512-sample-latency branch with a zero-latency branch to the sample: `cargo test -p ow-engine --test pdc`.
  - Benchmark runs and records results: `cargo bench -p ow-engine --bench tracks -- --quick` completes in CI (performance targets are estimates, tracked not gated).
- Status: pending
- Outcome:
- Handoff notes: coarse track-chain nodes; split only when a sidechain edge requires it.

## Stage 6: Transport, tempo map and timeline

- Objective: transport (play, stop, record arm, loop, seek, punch range), tempo map with ramps and time-signature map, beat/sample conversions, sample-accurate scheduling primitives for clip and event playback, metronome with count-in.
- Depends on: stage 4
- Owned paths: `crates/ow-engine/src/transport/`, `crates/ow-model/src/time/`
- Acceptance checks:
  - Conversion round trips and ramp integrals hold: `cargo test -p ow-model time` (including `proptest` cases).
  - Loop playback is seamless (no gap or duplicate sample at the loop point) in an offline render: `cargo test -p ow-engine --test transport`.
- Status: pending
- Outcome:
- Handoff notes: positions as `f64` beats plus 64-bit sample counters; 960 PPQ.

## Stage 7: Project model, edit commands, undo and serialization

- Objective: `ow-model` project structure (tracks, buses, clips, patterns and instances, media pool, devices, automation, markers), `EditCommand` with inverses and transactions, undo/redo, `.owave` directory bundle with atomic save, autosave to `backups/`, `format_version` and migration chain, `.owz` archive import/export, and the model-to-plan compiler in `ow-engine::compile`.
- Depends on: stage 6
- Owned paths: `crates/ow-model/`, `crates/ow-engine/src/compile.rs`
- Acceptance checks:
  - Apply-then-undo restores the model for random command sequences: `cargo test -p ow-model undo` (proptest).
  - Save/load round trip and golden v1 file load: `cargo test -p ow-model serde`.
  - Killing the process during save leaves the previous `project.json` intact (test simulates interrupted write): `cargo test -p ow-model atomic_save`.
- Status: pending
- Outcome:
- Handoff notes: the model has no engine dependency; the compiler lives in `ow-engine`.

## Stage 8: Audio and MIDI device layer and preferences

- Objective: `ow-io` device enumeration and selection for cpal hosts (WASAPI, CoreAudio, ALSA, PipeWire, JACK), sample-rate and buffer-size selection, WASAPI exclusive backend via `wasapi`, the off-by-default `asio` feature, MIDI input/output via `midir`, device-loss recovery, and a preferences window (audio, MIDI, paths, theme, shortcuts) persisted through the `paths` module.
- Depends on: stage 5
- Owned paths: `crates/ow-io/`, `src-tauri/src/prefs.rs`, `src/routes/settings/`
- Acceptance checks:
  - Default build has no ASIO code: `cargo tree -p ow-io -e features` shows no `asio` feature; `cargo deny check licenses` passes.
  - `cargo build -p ow-io --features asio` succeeds on the Windows CI runner (build only; binary not published).
  - Switching device and buffer size while playing recovers within one second without a crash (manual observation on Windows WASAPI shared and exclusive, macOS, Linux).
  - Unit tests for configuration negotiation pass: `cargo test -p ow-io`.
- Status: pending
- Outcome:
- Handoff notes: document in `docs/audio-devices.md` that ASIO-enabled self-builds carry GPLv3 obligations.

## Stage 9: IPC bridge and state mirror

- Objective: `ow-ipc` DTOs with `ts-rs` generation into `src/lib/ipc/generated/`, command handlers for edit commands, snapshot plus sequenced `ModelPatch` stream, UI stores mirroring the model, telemetry bank and 60 Hz raw-byte `ipc::Channel` for meters, transport position and CPU, and the peak-range command returning raw bytes.
- Depends on: stage 7
- Owned paths: `crates/ow-ipc/`, `src-tauri/src/ipc/`, `src/lib/ipc/`, `src/lib/stores/`
- Acceptance checks:
  - Generated TypeScript is current: `cargo test -p ow-ipc export_bindings` then `git diff --exit-code src/lib/ipc/generated` (CI).
  - Patch stream keeps the mirror equal to the model for random command sequences: `pnpm vitest run src/lib/stores` plus `cargo test -p ow-ipc mirror`.
  - Telemetry for 128 meters costs under 1% CPU at 60 Hz (estimate checked with the in-app CPU display; recorded in Outcome).
- Status: pending
- Outcome:
- Handoff notes: the UI never mutates the mirror; all edits go through commands.

## Stage 10: Media pool, decoding, peaks and disk streaming

- Objective: `ow-media` import (symphonia), resampling to project rate (rubato), content-hashed copy into `audio/`, peak pyramid files, disk-streaming reader with prefetch for long clips, in-memory playback for short clips, and the recording writer (hound) used by stage 12.
- Depends on: stage 7
- Owned paths: `crates/ow-media/`
- Acceptance checks:
  - Decode and resample fixtures (WAV, AIFF, FLAC, MP3, Ogg) to within tolerance: `cargo test -p ow-media decode`.
  - Streaming playback of a 30-minute file renders identically to in-memory playback: `cargo test -p ow-media stream`.
- Status: pending
- Outcome:
- Handoff notes: streaming threshold configurable (default 30 s).

## Stage 11: Arrangement view and audio clip editing

- Objective: canvas arrangement view (track headers, ruler with bars/beats and time, zoom and scroll, snap grid, markers), audio tracks with clip move, trim, split, duplicate, fades, gain, mute; waveform rendering from peaks; drag-and-drop import; keyboard shortcuts; playback of the arrangement through the engine.
- Depends on: stages 8, 9, 10
- Owned paths: `src/lib/arrange/`, `src/lib/canvas/`, `src/routes/+page.svelte` (main layout), `crates/ow-engine/src/clips/audio.rs`
- Acceptance checks:
  - Editing operations produce the expected model commands: `pnpm vitest run src/lib/arrange`.
  - Scrolling a 100-track, 500-clip project holds 60 fps in the Linux build (WebKitGTK) and on Windows (observed with the webview performance tools; recorded in Outcome).
  - An imported loop plays in time and an edited arrangement saves and reloads identically (manual journey).
- Status: pending
- Outcome:
- Handoff notes: profile on Linux first; it is the slowest webview.

## Stage 12: Audio recording

- Objective: input routing per track, record arm, input monitoring (with latency-compensated placement), punch in/out, loop recording into takes, basic comping (choose take per region), count-in, and the disk writer path.
- Depends on: stage 11
- Owned paths: `crates/ow-engine/src/record/`, `crates/ow-media/src/record.rs`, `src/lib/record/`
- Acceptance checks:
  - A loopback test records a known signal and places it within 1 sample of the expected position after latency compensation: `cargo test -p ow-engine --test record_loopback` (uses a virtual device).
  - Ten minutes of 16-track recording at 64-frame buffers shows no dropouts (manual observation with the xrun counter).
- Status: pending
- Outcome:
- Handoff notes: report device latency as given by the backend; offer a manual offset in preferences.

## Stage 13: MIDI clips, piano roll and MIDI recording

- Objective: MIDI tracks and clips, piano roll (draw, select, move, resize, velocity lane, CC lanes, scale highlighting, quantize, legato, humanize), MIDI recording (replace, overdub, loop-merge), MIDI file import/export with `midly`.
- Depends on: stage 11
- Owned paths: `src/lib/pianoroll/`, `crates/ow-engine/src/clips/midi.rs`, `crates/ow-model/src/midi/`
- Acceptance checks:
  - Quantize and MIDI file round trip tests pass: `cargo test -p ow-model midi`.
  - Piano roll command mapping tests pass: `pnpm vitest run src/lib/pianoroll`.
  - Recording from a MIDI keyboard places notes within 1 ms of played timing (manual observation).
- Status: pending
- Outcome:
- Handoff notes: until stage 14 exists, test MIDI playback with a sine test instrument.

## Stage 14: Device framework and built-in instruments

- Objective: `ow-dsp` `Device` trait (parameters with ranges, units, smoothing; note events; state; latency), parameter descriptors that drive both Svelte device panels and later plugin editors; instruments: subtractive synth (2 oscillators, sub, noise, filter, 2 envelopes, LFO, unison), FM synth (4 operators), and a sampler (one-shot and multisample zones, SFZ subset import, round-robin, velocity layers).
- Depends on: stage 13
- Owned paths: `crates/ow-dsp/src/device.rs`, `crates/ow-dsp/src/instruments/`, `src/lib/devices/`
- Acceptance checks:
  - Each instrument renders its reference patch within tolerance and passes the real-time safety harness: `cargo test -p ow-dsp instruments`.
  - SFZ subset fixture loads and maps zones correctly: `cargo test -p ow-dsp sfz`.
- Status: pending
- Outcome:
- Handoff notes: keep devices free of app types so stage 23 can export them unchanged.

## Stage 15: Step sequencer, drum machine and pattern workflow

- Objective: drum machine device (16 pads of sampler voices, choke groups), step sequencer (per-row steps, velocity, probability, swing, per-step pitch), pattern editor and playlist (pattern instances on tracks, make unique, pattern length and color), and the channel rack style view.
- Depends on: stage 14
- Owned paths: `crates/ow-dsp/src/instruments/drum.rs`, `crates/ow-model/src/pattern/`, `src/lib/stepseq/`, `src/lib/playlist/`
- Acceptance checks:
  - Editing a pattern changes every instance and "make unique" detaches one: `cargo test -p ow-model pattern`.
  - Swing and probability render deterministically with a fixed seed: `cargo test -p ow-dsp drum`.
- Status: pending
- Outcome:
- Handoff notes: patterns are ordinary clips to the engine compiler; no separate playback path.

## Stage 16: Mixer and routing

- Objective: mixer view with channel strips (insert slots, sends pre/post, pan with selectable pan law, fader, mute, solo, solo-safe, phase), buses and groups, sidechain inputs as graph edges, master chain, and meters fed by telemetry (peak, RMS, LUFS on master).
- Depends on: stages 9, 14
- Owned paths: `crates/ow-engine/src/mixer/`, `crates/ow-model/src/routing/`, `src/lib/mixer/`
- Acceptance checks:
  - Routing compile rejects feedback cycles and handles sidechain edges with PDC: `cargo test -p ow-engine --test routing`.
  - Pan laws and send levels match reference values: `cargo test -p ow-engine mixer`.
- Status: pending
- Outcome:
- Handoff notes: sidechain is an extra input bus on a device, not a special case.

## Stage 17: Built-in effects suite

- Objective: parametric EQ (8 bands, analyzer), compressor (with sidechain input), gate, limiter, algorithmic reverb, delay (sync, ping-pong), chorus, flanger, phaser, saturation/distortion, filter, bitcrusher, utility (gain, pan, width), tuner, spectrum analyzer; each with a Svelte panel from descriptors.
- Depends on: stage 16
- Owned paths: `crates/ow-dsp/src/effects/`, `src/lib/devices/effects/`
- Acceptance checks:
  - Each effect passes reference-render and real-time safety tests: `cargo test -p ow-dsp effects`.
  - Latency-reporting effects (limiter lookahead) are compensated: covered by `cargo test -p ow-engine --test pdc`.
- Status: pending
- Outcome:
- Handoff notes: may be split across two sessions (dynamics and EQ first, then time-based and modulation).

## Stage 18: Automation

- Objective: automation lanes for any device or mixer parameter, breakpoint envelopes with curves, sample-accurate application in the engine, write/touch/latch/read modes from UI controls and MIDI controllers, clip-level automation in patterns, and MIDI learn.
- Depends on: stages 16, 17
- Owned paths: `crates/ow-engine/src/automation/`, `crates/ow-model/src/automation/`, `src/lib/automation/`
- Acceptance checks:
  - A ramp automates gain sample-accurately in an offline render: `cargo test -p ow-engine --test automation`.
  - Touch and latch recording produce the expected breakpoints: `cargo test -p ow-model automation`.
- Status: pending
- Outcome:
- Handoff notes: thin recorded breakpoints with a tolerance to keep projects small.

## Stage 19: Smart tempo, time-stretch, loop browser and content packs

- Objective: warp markers and time-stretch with `signalsmith-stretch` (real-time and cached render), tempo and beat detection on import, follow-project-tempo for loops, key detection and transpose, a loop and sample browser with preview in tempo and key, tagging and search, and content-pack download and install into the data directory.
- Depends on: stages 10, 11
- Owned paths: `crates/ow-dsp/src/stretch/`, `crates/ow-dsp/src/analysis/`, `crates/ow-media/src/library/`, `src/lib/browser/`
- Acceptance checks:
  - Tempo detection is within 1 BPM on the fixture set: `cargo test -p ow-dsp analysis`.
  - Stretched loop length matches the target within 1 sample: `cargo test -p ow-dsp stretch`.
  - A loop dragged from the browser plays in project tempo and key (manual journey).
- Status: pending
- Outcome:
- Handoff notes: content source and licensing is an open owner question; ship the browser with a small CC0 starter pack if none is settled.

## Stage 20: Templates, export and project management

- Objective: start screen with recent projects and templates (genre templates, empty, "beat" pattern template), save-as-template, offline export (mix, stems per track or bus, selection or loop range, WAV 16/24/32f, FLAC, MIDI), loudness normalization option, and project consolidation (collect external media).
- Depends on: stages 15, 18
- Owned paths: `crates/ow-media/src/export/`, `crates/ow-engine/src/offline.rs` (export driver), `src/lib/start/`, `src/lib/export/`, `src-tauri/resources/templates/`
- Acceptance checks:
  - Exported WAV of the demo fixture matches the realtime-path render: `cargo test -p ow-engine --test render` (G4 fixture).
  - Stems sum to the mix within -90 dBFS: `cargo test -p ow-media export_stems`.
  - FLAC encoder dependency passes `cargo deny check licenses`.
- Status: pending
- Outcome:
- Handoff notes: choose the FLAC encoder crate here after verifying its license.

## Stage 21: Plugin hosting I: CLAP, scanner and plugin browser

- Objective: `ow-plugin-host` with the `HostedPlugin` trait, CLAP hosting via `clack-host` (audio, notes, parameters, state, latency, threading extensions, GUI), the out-of-process scanner (`--scan-plugin`), plugin cache and blacklist, plugin browser in the UI, native editor windows, and main-thread marshalling.
- Depends on: stages 14, 16
- Owned paths: `crates/ow-plugin-host/src/lib.rs`, `crates/ow-plugin-host/src/clap/`, `crates/ow-plugin-host/src/scan/`, `src-tauri/src/plugin_windows.rs`, `src/lib/plugins/`
- Acceptance checks:
  - Hosting tests load a test CLAP, process audio, round-trip state and parameters: `cargo test -p ow-plugin-host clap` on all three OSes in CI.
  - A deliberately crashing test plugin is blacklisted by the scanner without crashing the app: `cargo test -p ow-plugin-host scan_crash`.
  - A third-party CLAP (for example Surge XT) opens its editor and plays (manual, per OS).
- Status: pending
- Outcome:
- Handoff notes: until stage 23 produces OpenWave's own CLAPs, use clack's example plugins as test fixtures.

## Stage 22: Plugin hosting II: VST3

- Objective: VST3 hosting through the `vst3` crate: module loading and factory, component and controller (single and split), parameter changes and event lists, state (component and controller), latency and bus arrangements, `IPlugFrame` editors, VST3 bundle scanning paths per OS. Also the full G4 manual journey (record, arrange, mix, automate, export with built-in devices).
- Depends on: stage 21
- Owned paths: `crates/ow-plugin-host/src/vst3/`
- Acceptance checks:
  - VST3 hosting tests pass on all three OSes: `cargo test -p ow-plugin-host vst3` (CI) against the SDK's example plugins built in CI.
  - A third-party VST3 opens its editor, plays and restores state after project reload (manual, per OS).
  - G4 journey completed and recorded in Outcome.
- Status: pending
- Outcome:
- Handoff notes: after stage 23, switch fixtures to OpenWave's own VST3s.

## Stage 23: Plugin export: CLAP and VST3

- Objective: `plugins/ow-plugins` exporting every built-in instrument and effect through nice-plug (`vst3` feature uses the MIT `vst3` crate) as `.clap` and `.vst3`; `nice-plug-egui` editors generated from parameter descriptors; `xtask bundle-plugins`; CI runs `clap-validator` and the VST3 `validator`; `build.yml` packages `OpenWave-Plugins-<version>-<platform>.zip` and publish scripts attach it.
- Depends on: stages 17, 22
- Owned paths: `plugins/ow-plugins/`, `xtask/`, `.github/workflows/ci.yml` (validator job), `.github/workflows/build.yml` (plugin packaging), `scripts/ci/bundle-plugins.sh`, `docs/plugins.md`
- Acceptance checks:
  - `cargo tree -p ow-plugins -i vst3-sys` finds nothing and `cargo deny check licenses` passes (MIT-compatible VST3 path).
  - Validators pass for every exported plugin in CI on all three OSes.
  - In-app device and exported plugin render identically: `cargo test -p ow-plugins parity`.
  - The exported synth loads and plays in OpenWave's own host and in one third-party DAW (for example REAPER or Bitwig) (manual).
- Status: pending
- Outcome:
- Handoff notes: describe the format as "VST3" in text only; do not use the VST logo; check Steinberg's trademark guidelines before the first release that contains plugins.

## Stage 24: Plugin export: AUv2 on macOS

- Objective: `plugins/au-wrapper` CMake project using clap-wrapper to build `.component` bundles from the exported `.clap` files on the macOS runner, included in the macOS plugin zip; `auval` run in CI.
- Depends on: stage 23
- Owned paths: `plugins/au-wrapper/`, `scripts/ci/bundle-au.sh`, `.github/workflows/build.yml` (macOS AU step)
- Acceptance checks:
  - `auval -a` lists the OpenWave components and `auval -v` passes for each in the macOS CI job.
  - The exported synth loads in GarageBand or Logic (manual).
- Status: pending
- Outcome:
- Handoff notes: if clap-wrapper cannot build on the runner within one session, mark blocked and record why; the architecture record's revisit condition applies (macOS keeps CLAP and VST3).

## Stage 25: Plugin hosting III: AU on macOS and LV2 on Linux

- Objective: AU (v2 and v3) hosting through `objc2-audio-toolbox`/`objc2-avf-audio` with `AVAudioUnit` instantiation, parameters, state and view controllers; LV2 hosting through `livi` over system `lilv` (bundled into the AppImage), with atoms for MIDI, state, and UI via suil-free X11 embedding where supported (generic UI otherwise).
- Depends on: stage 22
- Owned paths: `crates/ow-plugin-host/src/au/`, `crates/ow-plugin-host/src/lv2/`, `scripts/ci/install-linux-deps.sh` (lilv), AppImage bundling config in `src-tauri/tauri.conf.json`
- Acceptance checks:
  - `cargo test -p ow-plugin-host au` passes on the macOS CI runner using Apple's built-in AUs (for example AUDelay).
  - `cargo test -p ow-plugin-host lv2` passes on the Linux CI runner using a packaged LV2 test plugin.
  - Apple's AUSampler and a third-party LV2 load and play in the app (manual).
- Status: pending
- Outcome:
- Handoff notes: may be split into two sessions (AU, LV2) if needed; LV2 GUI support may stay generic in this stage.
