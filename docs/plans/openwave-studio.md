# Plan: OpenWave Studio

Revision: 2026-11-22, revision 3 (critic rounds 1–3 applied; owner answers to open questions 1, 2 and 4 applied; no stage started)

Goals: each is true when its verifying command or observation succeeds.

- G1. The app builds and launches on Windows, macOS and Linux. Verify: `gh run list --workflow ci.yml --branch main --limit 1 --json conclusion` reports `success`; and the stage 2 launch procedure (download each OS's release bundle on a machine without dev tools, launch, main window visible within 10 s, test tone audible) passes on all three OSes, recorded in stage 2's Outcome.
- G2. Pushing a `v*` tag on a `main` commit publishes a stable release and the `main` feed; pushing to `experimental` publishes an experimental pre-release and the `experimental` feed. Verify: `gh release view updater-main --json assets` and `gh release view updater-experimental --json assets` each list `latest.json`, and the post-publish URL check (every URL in each `latest.json` returns HTTP 200) passes in the publish job.
- G3. An installed or portable build on either channel updates itself to the next build of the same channel. Verify: the stage 3 and stage 4 update procedures (install build N, publish N+1, app shows "update ready" within 2 minutes of a manual check, after restart Settings › About shows N+1) pass, recorded in those stages' Outcomes.
- G4. A song can be recorded, arranged, mixed, automated and exported using only built-in instruments and effects. Verify: `cargo test -p ow-engine --test demo_song` passes in CI (stage 23), and the stage 23 manual journey is recorded in its Outcome.
- G5. Third-party CLAP, VST3, AU (macOS) and LV2 (Linux) plugins load, play, automate and save state. Verify: `cargo test -p ow-plugin-host` passes in CI on all three OSes. AU tests compile only on macOS (`#[cfg(target_os = "macos")]`) and LV2 tests only on Linux (`#[cfg(target_os = "linux")]`), so the same command covers each OS's formats.
- G6. OpenWave's built-in devices load in other DAWs as CLAP and VST3 (and AUv2 on macOS). Verify: the `clap-validator`, VST3 `validator` and `auval` CI jobs pass, and `gh release view <tag> --json assets` lists `OpenWave-Plugins-<version>-<platform>.zip` for every platform.
- G7. Official binaries stay MIT-compatible. Verify: `cargo deny check licenses` passes in CI with GPL/LGPL/AGPL denied.

Scope / non-goals: scope is the application, its CI and release pipeline, the updater, and plugin hosting and export as described in the [architecture record](../architecture.md). Non-goals: VST2, AAX, mobile, cloud or collaboration, video tracks, notation, MP3 export, ASIO in official binaries, MSI installers, bundling the WebView2 runtime.

Assumptions:

- Repository `Densetsu152637/OpenWave-Studio` on GitHub with Actions enabled, branches `main` and `experimental`; work lands on `experimental` and `main` only takes merges from `experimental` (enforced from stage 2).
- The owner creates `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in stage 3.
- No Apple Developer ID: macOS app and plugin bundles are ad-hoc signed (`signingIdentity "-"`) and not notarized. Users clear Gatekeeper once per manual download (right-click › Open, or `xattr -dr com.apple.quarantine <path>`). Developer ID signing and notarization stay optional, used only if `APPLE_*` secrets are ever added.
- Toolchains: stable Rust, Node 24 with pnpm, Tauri CLI 2.x; versions at plan time are in the architecture record's sources.
- Builds and test suites run in CI (CI-first); implementers run only focused local checks. CI runners have no audio hardware, so realtime-path tests use the mock `AudioBackend` from stage 9.
- Manual checks: each states its procedure and threshold; the implementer records OS, build version, hardware and result in that stage's Outcome.

Relevant paths: `Cargo.toml`, `package.json`, `src/`, `src-tauri/`, `crates/*`, `plugins/*`, `xtask/`, `scripts/`, `.github/workflows/`, `docs/`. Reference CI: `D:\Git Repositories\Imperium-AI\.github\workflows\` and `D:\Git Repositories\Imperium-AI\scripts\ci\`.

Related docs: [docs/architecture.md](../architecture.md) wins on design decisions (crates, formats, licensing, IPC, threading); this plan wins on stage order, ownership and acceptance.

Open questions for the owner:

1. Source of loop and instrument content, and whether content packs are in scope for 1.0 (affects stage 22; does not block it).

Owner actions: generate the updater key pair and add both secrets (blocks stage 3).

Resolved 2026-11-22: ASIO stays source-build-only, no GPLv3 edition; plugin bundles ship on stable and experimental releases; no Apple Developer ID for now (ad-hoc signing, no notarization).

## Ownership rules and shared files

- The root `Cargo.toml` (stage 1) declares `members = ["src-tauri", "crates/*"]` (Cargo rejects member paths that do not exist yet), so new `crates/*` need no root edit; the stage that creates `plugins/ow-plugins` (27) or `xtask` appends it to `members` in that same commit. Each crate's own `Cargo.toml` is owned by the stage that creates the crate. `Cargo.lock` and `pnpm-lock.yaml` change in any stage that adds a dependency.
- Shared files are edited only in the section a stage names; later stages must not rewrite earlier stages' sections:

| Shared file | Stages and sections |
| --- | --- |
| `.github/workflows/build.yml` | 2 (matrix, tauri-action), 3 (signing secrets), 4 (portable signing), 27 (plugin packaging), 28 (macOS AU step), 30 (lilv in AppImage) |
| `.github/workflows/ci.yml` | 2 (all jobs), 27 (validator job), 28 (auval job) |
| `scripts/ci/publish-*.sh` | 2 (creates), 3 (feed upload), 27 (plugin zip attach) |
| `scripts/ci/install-linux-deps.sh` | 1 (creates: WebKitGTK 4.1, appindicator, librsvg, ALSA and JACK development packages), 2 (extends for bundling), 30 (lilv) |
| `src-tauri/tauri.conf.json` | 1 (creates), 2 (macOS signing identity, `infoPlist`, entitlements path), 3 (updater plugin), 30 (AppImage libs) |
| `src-tauri/Cargo.toml` | 1 (creates); any stage adding app dependencies |
| `src/routes/settings/` | 3 (updates panel), 9 (audio/MIDI/paths/theme/shortcuts panels) |
| `crates/ow-engine/tests/` | 5 (`render.rs`, `rt_safety.rs`), 6, 7, 14, 19, 21, 23 (each its own test file and fixtures) |
| `docs/releasing.md` | 2 (creates), 3 (feeds and keys), 4 (portable), 14 (macOS microphone re-prompt note) |
| `src-tauri/entitlements.plist` | 2 (creates, audio-input), 24 (library validation) |
| `docs/plugins.md` | 25 (hosting), 27 (export), 28 (AU license notes), 30 (lilv license notes) |

## How release CI mirrors Imperium-AI

| Imperium-AI | OpenWave Studio |
| --- | --- |
| `ci.yml`: PRs, pushes to `main`/`experimental`, merge queue; detect, docs, actionlint, rust, web, `source branch` (PRs into `main` only from `experimental`), `CI gate` | same jobs, plus `cargo deny`; plugin validators from stage 27 |
| `build.yml` reusable (`channel`, `version`), five-target matrix, `build-desktop.sh` calling `tauri build` | same matrix and inputs; `tauri-apps/tauri-action` v1 in build-only mode pinned by SHA; `collect-dist.sh` copies `artifactPaths`, builds and signs portable zips, fails on a missing `.sig` |
| `release.yml`: tags `v*`, `!v*-experimental*`; hyphen = pre-release | same, plus prepare fails unless the tagged commit is an ancestor of `origin/main` |
| `experimental.yml`: push to `experimental`; version `X.Y.<run>-experimental`; pre-release `--latest=false` | same, plus prepare fails unless the commit is an ancestor of `origin/experimental` |
| feed = `latest.json` of the newest channel release, found at runtime via the GitHub REST API | feed = fixed URL `releases/download/updater-<channel>/latest.json`; order release → feed → prune; `concurrency: updater-feed-<channel>`; no rollback of a newer feed; every feed URL checked for HTTP 200 |
| MSI and RPM skipped for hyphenated versions | NSIS only on Windows (no MSI); RPM skipped for hyphenated versions |
| Apple signing and notarization when `APPLE_*` secrets exist | macOS ad-hoc signed always; Developer ID signing and notarization only when `APPLE_*` secrets exist (none today) |
| — | plugin bundle zips attached to both stable and experimental releases (from stage 27) |

## Stage 1: Base app scaffold and engine smoke path

- Objective: a Tauri v2 app (Svelte 5 + TypeScript + Vite, pnpm) with the workspace layout from the architecture record; a minimal UI shell (menu bar, empty arrangement area, transport bar with play/stop, status bar with audio device and CPU); a minimal `ow-engine` that opens the default output through `cpal` and plays a 440 Hz tone on "Play test tone"; the portable `paths` module (marker detection, data directory, WebView2 data directory); `scripts/check-rust.sh` and `scripts/check-web.sh`; and a minimal `build-check.yml` workflow (fmt, clippy, test, `tauri build --no-bundle` on Windows, macOS and Linux; the Linux job runs `scripts/ci/install-linux-deps.sh`, created here, for the WebKitGTK and audio development packages) so the OS matrix is verified within this stage; a `--version` flag handled in `main()` before Tauri initializes (exits 0; prints the version when a console is available — on Windows it first calls `AttachConsole(ATTACH_PARENT_PROCESS)` because release builds use the `windows` subsystem) for headless launch smoke tests. The printed value is the build's effective version: CI injects it at build time (e.g. an `OW_VERSION` env var read via `option_env!`, falling back to `CARGO_PKG_VERSION`) so it matches channel versions set through `tauri build --config`.
- Depends on: none
- Owned paths: `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `package.json`, `pnpm-lock.yaml`, `pnpm-workspace.yaml`, `vite.config.ts`, `svelte.config.js`, `tsconfig.json`, `index.html`, `src/`, `src-tauri/` (incl. `Cargo.toml`, `tauri.conf.json`), `crates/ow-engine/Cargo.toml`, `crates/ow-engine/src/lib.rs`, `crates/ow-io/Cargo.toml`, `crates/ow-io/src/lib.rs`, `scripts/check-rust.sh`, `scripts/check-web.sh`, `scripts/ci/install-linux-deps.sh`, `.github/workflows/build-check.yml`, `README.md` (build section), `.gitignore`
- Acceptance checks:
  - OS matrix builds: `build-check.yml` succeeds on Windows, macOS and Linux for the stage's pushed commit (`gh run list --workflow build-check.yml --limit 1 --json conclusion` = `success`).
  - Web checks: `bash scripts/check-web.sh` exits 0 (in `build-check.yml`).
  - Launch and tone (manual, implementer's OS): `pnpm tauri dev`; window visible within 10 s; pressing "Play test tone" produces an audible tone within 1 s; status bar shows the device name. Record in Outcome.
  - Portable mode: `cargo test -p openwave-studio paths` exits 0 (covers marker present and absent).
  - Headless launch: the built binary run with `--version` exits 0 on all three OSes and its stdout equals the version in `tauri.conf.json` (Windows via the parent console attached by `AttachConsole`; step in `build-check.yml`). A unit test covers the version-string formatting.
- Status: done
- Outcome: `a2db2c3` (scaffold `d8daf75`, fixes `5b11550`, review fixes `a2db2c3`). `build-check.yml` runs 45002216940 and 45003101953 green on Windows, macOS, Linux (fmt, clippy, tests, `tauri build --no-bundle -- --locked`, headless `--version` = `0.1.0` on all three). `check-web.sh` and `cargo test -p openwave-studio paths` pass. Launch verified on Windows 11 via `pnpm tauri dev`: window up, default output opened (48 kHz, 2 ch). Test-tone audibility awaiting owner confirmation. Implementation-gate critic: no acceptance-changing findings; 3 improvements adopted.
- Handoff notes: keep the smoke path behind the `Engine` handle later stages extend. App identifier `org.openwave.studio`, product name `OpenWave Studio`. Stage 2 deletes `build-check.yml` when `ci.yml` supersedes it. Engine API: `ow_engine::Engine::start()` handle with `output()`, `set_test_tone`, `cpu_load()`; `ow_io::OutputStream::open_default(render)` runs the cpal stream on its own thread. The main window is built in `setup()` so the portable WebView2 `data_directory` applies. `[workspace.package] version` must equal `tauri.conf.json` version (unit-tested). The CI rust job runs `pnpm build` before cargo because `generate_context!` embeds `dist/`. Placeholder icons in `src-tauri/icons/`. Known gaps for later stages: stream errors are only logged (stage 9 adds an error state and device-loss handling; the stream uses the device default config only); macOS may ignore `data_directory`, leaving WebKit storage outside the portable folder (stage 4 verifies and documents).

## Stage 2: Release CI

- Objective: CI and release workflows mirroring Imperium-AI as tabled above: `ci.yml`, `build.yml`, `release.yml`, `experimental.yml`, and `scripts/ci/{detect,install-linux-deps,release-prepare,experimental-prepare,collect-dist,publish-release,publish-experimental,prune-releases}.sh` plus `check-links.py`; `deny.toml`; `.markdownlint-cli2.jsonc`; create the `experimental` branch. Prepare jobs check out with `fetch-depth: 0` and enforce branch ancestry (`git merge-base --is-ancestor "$GITHUB_SHA" origin/main` for stable tags, `origin/experimental` for experimental builds). Resolve and pin by SHA every action, including the commit of tauri-action's chosen `action-v1.x.y` release tag (its tags are named `action-vX.Y.Z`). Portable artifacts without updater signatures at this stage: Windows plain-exe zip with `portable` marker, macOS `.app` zip with marker, Linux AppImage. macOS bundles are ad-hoc signed (`bundle.macOS.signingIdentity: "-"`); the Developer ID/notarization path is wired but runs only when `APPLE_*` secrets are present. README and `docs/releasing.md` document the Gatekeeper workaround (right-click › Open, or `xattr -dr com.apple.quarantine`). macOS bundle config gains `NSMicrophoneUsageDescription` and an entitlements file with `com.apple.security.device.audio-input` (checked in stage 14). `docs/releasing.md` also documents the manual `force_feed` override for an intentional version reset. Delete `build-check.yml`.
- Depends on: stage 1
- Owned paths: `.github/workflows/` (creates `ci.yml`, `build.yml`, `release.yml`, `experimental.yml`; deletes `build-check.yml`), `scripts/ci/`, `deny.toml`, `.markdownlint-cli2.jsonc`, `docs/releasing.md`, `src-tauri/tauri.conf.json` (`bundle.macOS.signingIdentity`, `bundle.macOS.infoPlist`, `bundle.macOS.entitlements`), `src-tauri/Info.plist` (`NSMicrophoneUsageDescription`), `src-tauri/entitlements.plist` (creates, with `com.apple.security.device.audio-input`), `README.md` (install section)
- Acceptance checks:
  - CI green on a PR: `gh pr checks <pr>` shows `CI gate` passing, with rust jobs on Windows, macOS and Linux.
  - Source-branch rule: a PR from a feature branch into `main` fails `source branch` (observed once, then closed).
  - Ancestry rule: pushing tag `v0.0.0-ancestry-test` on a commit not on `main` makes `release.yml`'s `prepare` fail (`gh run view <id>` shows prepare failed); the tag is then deleted.
  - Experimental release: a push to `experimental` produces pre-release `vX.Y.N-experimental` with installers, portable zips and `SHA256SUMS` for all five matrix targets (`gh release view vX.Y.N-experimental --json assets`).
  - Stable release: tag `v0.1.0` on `main` produces a release marked latest with the same asset set.
  - Ad-hoc signature: in the macOS build job, `codesign -dv "OpenWave Studio.app"` reports `Signature=adhoc`.
  - macOS launch smoke: the macOS build job runs `"OpenWave Studio.app/Contents/MacOS/OpenWave Studio" --version` on the signed bundle and it exits 0; this check asserts the exit code only, not the printed version (catches signing or entitlement errors that kill the process at load).
  - G1 launch procedure: download the `v0.1.0` NSIS installer, the `.dmg` and the AppImage through a browser on machines without dev tools, launch (on macOS using the documented right-click › Open workaround); window visible within 10 s and test tone audible on each. Record per OS in Outcome.
  - `actionlint` and `cargo deny check licenses` pass in CI.
- Status: pending
- Outcome:
- Handoff notes: no signing key yet, so `createUpdaterArtifacts` is false and no feed is written. `docs/releasing.md` documents tag format, channels, ancestry rule and required secrets. Before wiring the macOS config, verify the exact key names (`bundle.macOS.infoPlist`, `bundle.macOS.entitlements`, `bundle.macOS.signingIdentity`) against the Tauri v2 config schema for the pinned Tauri version, and whether Tauri applies the hardened runtime under ad-hoc signing (it affects whether the library-validation entitlement matters before Developer ID signing); record both findings in Outcome.

## Stage 3: Auto-updater for installed builds (main and experimental)

- Objective: `tauri-plugin-updater` with the public key; signing secrets in `build.yml`; `scripts/ci/updater-manifest.sh` writing `latest.json` with every platform key from the architecture record (`windows-x86_64[-nsis]`, `windows-aarch64[-nsis]`, `darwin-x86_64`, `darwin-aarch64`, `linux-x86_64[-appimage]`, `linux-aarch64[-appimage]`); publish scripts doing release → feed upload (`gh release upload --clobber` to fixed releases `updater-main` / `updater-experimental`, created if absent) → prune, under `concurrency: updater-feed-<channel>` (`cancel-in-progress: false`), skipping the upload if the existing feed's version is greater, then checking every feed URL for HTTP 200. Manual override: `experimental.yml` gains a `force_feed` boolean `workflow_dispatch` input, and `release.yml` gains `workflow_dispatch` with the same input, accepted only when run on a tag ref (`github.ref_type == 'tag'`; prepare fails otherwise); `force_feed` skips only the "newer feed wins" check and never the branch-ancestry check. App side, `src-tauri/src/update.rs`: channel default from version, switch in settings, checks 20 s after start and every 6 h, background download, install on quit or "restart now"; after a channel switch exactly one check uses a "any different version" comparator, not persisted; Linux package-managed detection (`$APPIMAGE` unset) disables checks and shows "Update OpenWave Studio through your package manager".
- Depends on: stage 2
- Owned paths: `src-tauri/src/update.rs`, `src-tauri/tauri.conf.json` (updater section), `src-tauri/capabilities/updater.json`, `src/lib/update/`, `src/routes/settings/update/`, `scripts/ci/updater-manifest.sh`, `scripts/ci/publish-release.sh`, `scripts/ci/publish-experimental.sh`, `scripts/ci/check-feed.sh`, `.github/workflows/build.yml` (signing env), `.github/workflows/release.yml` and `experimental.yml` (publish concurrency, `workflow_dispatch` `force_feed` input), `docs/releasing.md` (feeds and keys)
- Acceptance checks:
  - Feed contents: `curl -sL https://github.com/Densetsu152637/OpenWave-Studio/releases/download/updater-experimental/latest.json | jq -r '.platforms | keys[]'` lists all ten installer keys above.
  - Feed URLs live: the publish job's `check-feed.sh` step passes (every URL in `latest.json` returns HTTP 200) for one experimental and one stable publish.
  - No rollback: dispatching an older experimental build with the default `force_feed=false` leaves the feed version unchanged (`jq .version` before and after).
  - Override: the same dispatch with `force_feed=true` changes the feed's `jq .version` to the older build's version; a `release.yml` dispatch on a branch ref fails in prepare, and a forced dispatch on a tag whose commit is not on `main` still fails the ancestry check. Afterwards the feed is restored with a normal publish.
  - Installed update procedure (Windows NSIS and one of macOS/Linux AppImage): install experimental build N, publish N+1, press "Check now"; "update ready" appears within 2 minutes; after "restart now", Settings › About shows N+1. Record in Outcome.
  - Channel switch: switching an experimental install to `main` installs the current stable once; the next check with no newer stable reports up to date. Record in Outcome.
  - Package-managed: the deb install shows the package-manager message and makes no request to the feed (observed in app log).
  - Unit tests for channel derivation, feed URLs, comparator scope and package-managed detection: `cargo test -p openwave-studio update` (CI).
- Status: pending
- Outcome:
- Handoff notes: the owner generates keys with `pnpm tauri signer generate` and adds both secrets; the agent never handles the private key.

## Stage 4: Portable self-update and macOS translocation

- Objective: `collect-dist.sh` signs portable zips with `tauri signer sign` and fails if any updater artifact or portable zip lacks a `.sig`; feed gains `windows-x86_64-portable` and `windows-aarch64-portable`; `src-tauri/src/portable_update.rs` implements the Windows portable path (custom target check, verified `download()`, staging, `--apply-update` helper relaunched from temp and handled in `main()` before Tauri initializes; the swap renames each old top-level entry to `.bak`, moves new entries in, and restores everything on any error; `OpenWave Studio Data/`, the `portable` marker and `.update-staging/` are never touched; each rename is retried with exponential backoff (starting at 100 ms) while the total stays within the 30 s budget, to ride out antivirus and indexer locks; staged entries named `portable` or `OpenWave Studio Data` are skipped on move-in so a malformed package cannot overwrite them; relaunch via `std::process::Command`); macOS translocation detection with a blocking prompt to move the app or clear quarantine (every browser-downloaded copy is quarantined because builds are not notarized, so this path is the normal first-run experience on macOS); `docs/portable.md` with the Gatekeeper and quarantine workaround.
- Depends on: stage 3
- Owned paths: `src-tauri/src/portable_update.rs`, `src-tauri/src/translocation.rs`, `scripts/ci/collect-dist.sh` (signing section), `scripts/ci/updater-manifest.sh` (portable keys), `.github/workflows/build.yml` (portable signing env), `docs/portable.md`, `docs/releasing.md` (portable section)
- Acceptance checks:
  - Signatures complete: the `collect-dist.sh` check passes in the build job, and `gh release view <tag> --json assets` shows a `.sig` for every portable zip.
  - Windows portable update procedure: unzip build N to a folder with the marker, create a setting, publish N+1, "Check now"; within 2 minutes "update ready"; after restart About shows N+1 and the setting persists. Record in Outcome.
  - Linux AppImage and macOS `.app` portable updates: same procedure, marker beside the AppImage / `.app`. Record in Outcome.
  - Translocation: download the macOS portable zip through a browser (quarantined), unzip, launch in place; the app shows the move prompt and does not enable portable mode; after moving the app with its marker to another folder (or `xattr -dr com.apple.quarantine`) and relaunching, portable mode is on and an update applies. Record in Outcome.
  - Swap safety: `cargo test -p openwave-studio portable_update` covers the swap plan, exclusion of `OpenWave Studio Data/` and `portable`, an injected failure after half the entries are moved (asserting the folder afterwards is byte-identical to the old version), a staged package containing a `portable` entry (asserting the existing marker is untouched), and a rename that fails twice then succeeds (asserting the swap completes).
  - Translocation detection: `cargo test -p openwave-studio translocation`.
- Status: pending
- Outcome:
- Handoff notes: the apply helper must tolerate locked files (main process exit, antivirus) for up to 30 s in total, then restore and keep the old version.

## Stage 5: Engine graph core (single-threaded)

- Objective: `ow-engine` audio graph: `Node` trait, buffer pool, `RenderPlan`, a control → audio SPSC ring (`rtrb`) carrying plans and parameter/transport messages, old-plan return ring, a generic `ProducerRing` API so later stages add one ring per producer, overflow counter, offline render driver, and the RT-safety harness (`assert_no_alloc` around each cycle in debug and test builds).
- Depends on: stage 1
- Owned paths: `crates/ow-engine/src/graph/`, `crates/ow-engine/src/plan.rs`, `crates/ow-engine/src/rings.rs`, `crates/ow-engine/src/offline.rs`, `crates/ow-engine/tests/render.rs`, `crates/ow-engine/tests/rt_safety.rs`, `crates/ow-engine/tests/fixtures/graphs/`
- Acceptance checks:
  - Offline render of oscillator → gain → master matches the reference within -90 dBFS: `cargo test -p ow-engine --test render`.
  - No allocation in the cycle across 1000 plan swaps and with ring overflow: `cargo test -p ow-engine --test rt_safety`.
- Status: pending
- Outcome:
- Handoff notes: document `Node`, `ProcessContext`, `RenderPlan` and `ProducerRing` with rustdoc; every later engine stage consumes them.

## Stage 6: Multithreaded scheduler and delay compensation

- Objective: DAG scheduler (per-node atomic dependency counters, lock-free ready queue), real-time worker pool (`audio_thread_priority`), user-set worker count, macOS audio-workgroup join for workers, PDC in plan compilation, `criterion` benchmarks including the track-count benchmark.
- Depends on: stage 5
- Owned paths: `crates/ow-engine/src/sched/`, `crates/ow-engine/src/pdc.rs`, `crates/ow-engine/src/workgroup.rs`, `crates/ow-engine/benches/`, `crates/ow-engine/tests/determinism.rs`, `crates/ow-engine/tests/pdc.rs`
- Acceptance checks:
  - Bit-identical renders for 1, 2 and 8 workers: `cargo test -p ow-engine --test determinism`.
  - RT safety with 8 workers: `OW_TEST_WORKERS=8 cargo test -p ow-engine --test rt_safety` passes (no allocation on any worker or the callback thread).
  - PDC aligns a 512-sample-latency branch with a zero-latency branch to the sample: `cargo test -p ow-engine --test pdc`.
  - macOS workgroup: on the macOS CI runner, `cargo test -p ow-engine workgroup` asserts the join call succeeds for a workgroup obtained from the default device, or is skipped with a logged reason when the runner exposes no device.
  - Benchmarks run: `cargo bench -p ow-engine --bench tracks -- --quick` completes in CI (tracked, not gated).
- Status: pending
- Outcome:
- Handoff notes: coarse track-chain nodes; split only where a sidechain edge requires it.

## Stage 7: Transport, tempo map and timeline

- Objective: transport (play, stop, record arm, loop, seek, punch range), tempo map with ramps and time-signature map, beat/sample conversions, sample-accurate scheduling primitives, metronome with count-in.
- Depends on: stage 5
- Owned paths: `crates/ow-engine/src/transport/`, `crates/ow-engine/tests/transport.rs`, `crates/ow-model/Cargo.toml`, `crates/ow-model/src/lib.rs`, `crates/ow-model/src/time/`
- Acceptance checks:
  - Conversion round trips and ramp integrals hold: `cargo test -p ow-model time` (with `proptest`).
  - Loop playback has no gap or duplicate sample at the loop point: `cargo test -p ow-engine --test transport`.
- Status: pending
- Outcome:
- Handoff notes: positions as `f64` beats plus 64-bit sample counters; 960 PPQ.

## Stage 8: Project model, edit commands, undo and serialization

- Objective: `ow-model` project structure (tracks, buses, clips, patterns and instances, media pool, devices, automation, markers), `EditCommand` with inverses and transactions, undo/redo, `.owave` bundle with atomic save, autosave to `backups/`, `format_version` and migration chain, `.owz` import/export, and the model-to-plan compiler `ow-engine::compile`.
- Depends on: stage 7
- Owned paths: `crates/ow-model/src/` (except `time/`), `crates/ow-model/tests/`, `crates/ow-engine/src/compile.rs`
- Acceptance checks:
  - Apply-then-undo restores the model for random command sequences: `cargo test -p ow-model undo`.
  - Save/load round trip and golden v1 file load: `cargo test -p ow-model serde`.
  - Interrupted write leaves the previous `project.json` intact: `cargo test -p ow-model atomic_save`.
- Status: pending
- Outcome:
- Handoff notes: the model never depends on the engine.

## Stage 9: Audio and MIDI device layer, mock backend and preferences

- Objective: `ow-io` `AudioBackend` trait; cpal hosts (WASAPI, CoreAudio, ALSA, PipeWire, JACK); WASAPI exclusive backend via `wasapi`; off-by-default `asio` feature; the **mock backend** (thread- or test-driven callback, fixed buffer size, injected input, timestamped MIDI, loopback with set latency); MIDI I/O via `midir` with one SPSC ring per MIDI input callback thread and one for control-surface input; device-loss recovery; preferences (audio, MIDI, paths, theme, shortcuts); `docs/audio-devices.md` (ASIO GPLv3 note for self-builds).
- Depends on: stage 6
- Owned paths: `crates/ow-io/src/` (incl. `mock.rs`), `crates/ow-io/tests/`, `src-tauri/src/prefs.rs`, `src/routes/settings/` (except `update/`), `docs/audio-devices.md`
- Acceptance checks:
  - Default build has no ASIO: `cargo tree -p ow-io -e features` shows no `asio`; `cargo deny check licenses` passes.
  - `cargo build -p ow-io --features asio` succeeds on the Windows CI runner (build only, not published).
  - Mock backend drives a plan through 10 000 callbacks at 64 frames with no allocation, and MIDI from two producer threads arrives with no loss: `cargo test -p ow-io mock`.
  - Device switch (manual, Windows shared and exclusive, macOS, Linux): change device and buffer size while playing; audio resumes within 1 s with no crash. Record in Outcome.
- Status: pending
- Outcome:
- Handoff notes: the mock backend is the realtime path for all later CI tests.

## Stage 10: IPC bridge and state mirror

- Objective: `ow-ipc` DTOs with `ts-rs` generation into `src/lib/ipc/generated/`, edit-command handlers, snapshot plus sequenced `ModelPatch` stream with resync on gap, UI stores mirroring the model, telemetry bank and 60 Hz raw-byte `ipc::Channel`, peak-range command returning raw bytes.
- Depends on: stage 8
- Owned paths: `crates/ow-ipc/`, `src-tauri/src/ipc/`, `src/lib/ipc/`, `src/lib/stores/`
- Acceptance checks:
  - Generated TypeScript is current: `cargo test -p ow-ipc export_bindings && git diff --exit-code src/lib/ipc/generated` (CI).
  - Mirror equals model for random command sequences, including a forced sequence gap: `cargo test -p ow-ipc mirror` and `pnpm vitest run src/lib/stores`.
  - Telemetry budget: `cargo test -p ow-ipc --release telemetry_budget` asserts that sampling and packing 128 meters plus transport takes under 100 µs median over 1000 iterations (well under the 16.7 ms frame).
- Status: pending
- Outcome:
- Handoff notes: the UI never mutates the mirror.

## Stage 11: Media pool, decoding, peaks and disk streaming

- Objective: `ow-media` import (symphonia), resampling (rubato), content-hashed copy into `audio/`, peak pyramid files, disk-streaming reader with prefetch, in-memory playback for short clips, recording writer (hound).
- Depends on: stage 8
- Owned paths: `crates/ow-media/`
- Acceptance checks:
  - Decode and resample fixtures (WAV, AIFF, FLAC, MP3, Ogg) within tolerance: `cargo test -p ow-media decode`.
  - Streaming a 30-minute file renders identically to in-memory playback: `cargo test -p ow-media stream`.
- Status: pending
- Outcome:
- Handoff notes: streaming threshold configurable (default 30 s).

## Stage 12: Arrangement view and navigation

- Objective: canvas arrangement view: track headers (add, rename, color, reorder), ruler (bars/beats and time), zoom and scroll, snap grid, markers, selection model, keyboard shortcuts, playhead following transport telemetry; a dev-only performance hook `window.__owPerf.scrollBenchmark()` that scrolls for 10 s and reports p95 frame time.
- Depends on: stages 9, 10
- Owned paths: `src/lib/arrange/` (view, headers, ruler), `src/lib/canvas/`, `src/lib/perf/`, `src/routes/+page.svelte`, `crates/ow-model/tests/fixtures/100_tracks.owave/`
- Acceptance checks:
  - View-model tests (zoom, snap, selection): `pnpm vitest run src/lib/arrange`.
  - Frame budget (manual): open the `100_tracks.owave` fixture (100 tracks, 500 clips), run the scroll benchmark; p95 frame time ≤ 16.7 ms on Windows, on Linux under an X11 session, and on Linux under a Wayland session (the configuration where plugin editors use XWayland). Record OS, GPU and p95 in Outcome.
- Status: pending
- Outcome:
- Handoff notes: profile WebKitGTK first; it is the slowest webview.

## Stage 13: Audio clip editing and arrangement playback

- Objective: audio tracks with clip move, trim, split, duplicate, fades, gain and mute; waveform rendering from peaks; drag-and-drop import; arrangement playback through the engine.
- Depends on: stages 11, 12
- Owned paths: `src/lib/arrange/clips/`, `src/lib/waveform/`, `crates/ow-engine/src/clips/audio.rs`, `crates/ow-engine/tests/audio_clips.rs`
- Acceptance checks:
  - Editing operations emit the expected commands: `pnpm vitest run src/lib/arrange/clips`.
  - Clip playback with fades and trims matches the reference via the mock backend: `cargo test -p ow-engine --test audio_clips`.
  - Save and reload of an edited arrangement is identical: covered by `cargo test -p ow-model serde` fixtures added here.
- Status: pending
- Outcome:
- Handoff notes: none.

## Stage 14: Audio recording

- Objective: per-track input routing, record arm, latency-compensated input monitoring and placement, punch in/out, loop recording into takes, basic comping, count-in, disk-writer path.
- Depends on: stage 13
- Owned paths: `crates/ow-engine/src/record/`, `crates/ow-engine/tests/record_loopback.rs`, `crates/ow-media/src/record.rs`, `src/lib/record/`, `docs/releasing.md` (macOS microphone re-prompt note)
- Acceptance checks:
  - Loopback through the mock backend (known latency) places a recorded impulse within 1 sample of its expected position: `cargo test -p ow-engine --test record_loopback`.
  - Sustained recording: the mock backend runs 16 input tracks for 10 simulated minutes at 64 frames with the disk writer active and reports zero ring overruns: `cargo test -p ow-engine --test record_loopback sustained --release`.
  - macOS microphone (manual): on a macOS machine that has never run OpenWave, the first record press with the ad-hoc signed build shows the system microphone prompt naming OpenWave Studio; after allowing, a 5 s recording from the built-in microphone peaks above -60 dBFS. `codesign -d --entitlements -` on the app shows `com.apple.security.device.audio-input` and `plutil -p` on its Info.plist shows `NSMicrophoneUsageDescription`. Record in Outcome, including whether a later update re-prompted.
- Status: pending
- Outcome:
- Handoff notes: offer a manual latency offset in preferences.

## Stage 15: MIDI clips, piano roll and MIDI recording

- Objective: MIDI tracks and clips; piano roll (draw, select, move, resize, velocity and CC lanes, scale highlighting, quantize, legato, humanize); MIDI recording (replace, overdub, loop-merge); MIDI file import/export with `midly`.
- Depends on: stage 13
- Owned paths: `src/lib/pianoroll/`, `crates/ow-engine/src/clips/midi.rs`, `crates/ow-engine/tests/midi_timing.rs`, `crates/ow-model/src/midi/`
- Acceptance checks:
  - Quantize and MIDI file round trip: `cargo test -p ow-model midi`.
  - Piano roll command mapping: `pnpm vitest run src/lib/pianoroll`.
  - Recording timing: timestamped synthetic MIDI events injected through the mock backend at random offsets land within 1 sample of their timestamps in the recorded clip: `cargo test -p ow-engine --test midi_timing`.
- Status: pending
- Outcome:
- Handoff notes: use a sine test instrument until stage 16.

## Stage 16: Device framework and synthesizers

- Objective: `ow-dsp` `Device` trait (parameters with ranges, units, smoothing; note events; state; latency; sidechain inputs), parameter descriptors that drive Svelte device panels and later plugin editors; subtractive synth (2 oscillators, sub, noise, filter, 2 envelopes, LFO, unison) and FM synth (4 operators).
- Depends on: stage 15
- Owned paths: `crates/ow-dsp/Cargo.toml`, `crates/ow-dsp/src/lib.rs`, `crates/ow-dsp/src/device.rs`, `crates/ow-dsp/src/instruments/{synth,fm}.rs`, `src/lib/devices/` (panel framework)
- Acceptance checks:
  - Reference patches render within tolerance and pass the RT-safety harness: `cargo test -p ow-dsp instruments`.
- Status: pending
- Outcome:
- Handoff notes: devices must not depend on app types so stage 27 can export them unchanged.

## Stage 17: Sampler and SFZ import

- Objective: sampler instrument (one-shot and multisample zones, round-robin, velocity layers, loop points, envelopes) and an SFZ subset importer.
- Depends on: stage 16
- Owned paths: `crates/ow-dsp/src/instruments/sampler/`, `src/lib/devices/sampler/`
- Acceptance checks:
  - Zone mapping, round-robin and velocity layers: `cargo test -p ow-dsp sampler`.
  - SFZ subset fixture maps zones correctly: `cargo test -p ow-dsp sfz`.
- Status: pending
- Outcome:
- Handoff notes: document the supported SFZ opcodes in rustdoc.

## Stage 18: Step sequencer, drum machine and pattern workflow

- Objective: drum machine device (16 sampler pads, choke groups); step sequencer (steps, velocity, probability, swing, per-step pitch); pattern editor and playlist (pattern instances, make unique, pattern length and color); channel-rack view.
- Depends on: stage 17
- Owned paths: `crates/ow-dsp/src/instruments/drum.rs`, `crates/ow-model/src/pattern/`, `src/lib/stepseq/`, `src/lib/playlist/`
- Acceptance checks:
  - Editing a pattern changes every instance; "make unique" detaches one: `cargo test -p ow-model pattern`.
  - Swing and probability are deterministic with a fixed seed: `cargo test -p ow-dsp drum`.
- Status: pending
- Outcome:
- Handoff notes: patterns are ordinary clips to the compiler.

## Stage 19: Mixer and routing

- Objective: mixer view (insert slots, pre/post sends, pan with selectable law, fader, mute, solo, solo-safe, phase), buses and groups, sidechain inputs as graph edges, master chain, meters (peak, RMS, LUFS on master).
- Depends on: stages 10, 16
- Owned paths: `crates/ow-engine/src/mixer/`, `crates/ow-engine/tests/routing.rs`, `crates/ow-model/src/routing/`, `src/lib/mixer/`
- Acceptance checks:
  - Feedback cycles rejected; sidechain edges compensated: `cargo test -p ow-engine --test routing`.
  - Pan laws and send levels match reference values: `cargo test -p ow-engine mixer`.
- Status: pending
- Outcome:
- Handoff notes: sidechain is an extra input bus on a device.

## Stage 20: Built-in effects suite

- Objective: parametric EQ (8 bands, analyzer), compressor (sidechain), gate, limiter, algorithmic reverb, delay (sync, ping-pong), chorus, flanger, phaser, saturation, filter, bitcrusher, utility, tuner, spectrum analyzer; panels from descriptors.
- Depends on: stage 19
- Owned paths: `crates/ow-dsp/src/effects/`, `src/lib/devices/effects/`
- Acceptance checks:
  - Reference renders and RT safety per effect: `cargo test -p ow-dsp effects`.
  - Limiter lookahead is compensated: `cargo test -p ow-engine --test pdc` (fixture added here).
- Status: pending
- Outcome:
- Handoff notes: Minimum accepted slice: EQ, compressor, limiter, reverb, delay and utility with passing tests; remaining effects may follow in a second session under the same stage.

## Stage 21: Automation

- Objective: automation lanes for any device or mixer parameter (addressed by device id and parameter id, so hosted plugins reuse it), breakpoint envelopes with curves, sample-accurate application, write/touch/latch/read modes from UI and MIDI controllers (control-surface ring from stage 9), clip automation in patterns, MIDI learn.
- Depends on: stages 19, 20
- Owned paths: `crates/ow-engine/src/automation/`, `crates/ow-engine/tests/automation.rs`, `crates/ow-model/src/automation/`, `src/lib/automation/`
- Acceptance checks:
  - A ramp automates gain sample-accurately: `cargo test -p ow-engine --test automation`.
  - Touch and latch recording produce the expected breakpoints: `cargo test -p ow-model automation`.
- Status: pending
- Outcome:
- Handoff notes: thin recorded breakpoints with a tolerance.

## Stage 22: Smart tempo, time-stretch, loop browser and content packs

- Objective: warp markers and time-stretch with `signalsmith-stretch` (real-time and cached), tempo and beat detection on import, follow-project-tempo loops, key detection and transpose, loop/sample browser with in-tempo preview, tags and search, content-pack download and install.
- Depends on: stages 11, 13
- Owned paths: `crates/ow-dsp/src/stretch/`, `crates/ow-dsp/src/analysis/`, `crates/ow-media/src/library/`, `src/lib/browser/`
- Acceptance checks:
  - Tempo detection within 1 BPM on the fixture set: `cargo test -p ow-dsp analysis`.
  - Stretched loop length within 1 sample of target: `cargo test -p ow-dsp stretch`.
  - Browser preview (manual): a 120 BPM fixture loop previewed in a 90 BPM project plays in time (no drift over 8 bars by ear against the metronome). Record in Outcome.
- Status: pending
- Outcome:
- Handoff notes: Minimum accepted slice: stretch, tempo detection and the browser with local folders; content-pack download may follow in a second session. Content source is open question 3.

## Stage 23: Templates, export, project management and demo song (G4)

- Objective: start screen with recent projects and templates, save-as-template, offline export (mix, stems, selection or loop range, WAV 16/24/32f, FLAC, MIDI), loudness normalization, project consolidation; the `demo_song.owave` fixture (built-in synth, sampler drums pattern, recorded audio clip, two effects, a bus with sidechain compression, automation) and `--test demo_song` comparing the realtime path (mock backend callback, 128-frame buffers) with offline export.
- Depends on: stages 14, 18, 21
- Owned paths: `crates/ow-media/src/export/`, `crates/ow-engine/src/export.rs`, `crates/ow-engine/tests/demo_song.rs`, `crates/ow-engine/tests/fixtures/demo_song.owave/`, `src/lib/start/`, `src/lib/export/`, `src-tauri/resources/templates/`
- Acceptance checks:
  - Realtime path equals export: `cargo test -p ow-engine --test demo_song` (difference below -90 dBFS).
  - Stems sum to the mix within -90 dBFS: `cargo test -p ow-media export_stems`.
  - FLAC encoder dependency passes `cargo deny check licenses`.
  - G4 journey (manual): from the empty template, record 8 bars of audio, program a drum pattern and a synth part, add EQ and reverb, automate a filter, export WAV; the exported file plays in a system media player and matches what was heard. Record in Outcome.
- Status: pending
- Outcome:
- Handoff notes: choose the FLAC encoder after verifying its license.

## Stage 24: Plugin hosting I: CLAP processing core

- Objective: `ow-plugin-host` `HostedPlugin` trait; CLAP hosting via `clack-host` (audio, note ports, parameters, state, latency, thread-check and thread-pool extensions); hosted parameters exposed to automation; plugin latency fed into PDC; the `MainThreadExecutor` abstraction (Windows/macOS: Tauri main thread via `run_on_main_thread`; Linux: a dedicated plugin main thread that stage 25 turns into the editor run loop), which also answers the host's `is_main_thread`; adds `com.apple.security.cs.disable-library-validation` to the entitlements file created in stage 2.
- Depends on: stages 16, 19, 21
- Owned paths: `crates/ow-plugin-host/Cargo.toml`, `crates/ow-plugin-host/src/lib.rs`, `crates/ow-plugin-host/src/main_thread.rs`, `crates/ow-plugin-host/src/clap/`, `crates/ow-plugin-host/tests/clap.rs`, `crates/ow-plugin-host/tests/main_thread.rs`, `crates/ow-plugin-host/tests/fixtures/`, `src-tauri/entitlements.plist` (library-validation key only)
- Acceptance checks:
  - Load, process, parameter and state round trip on all three OSes: `cargo test -p ow-plugin-host clap` (CI).
  - Hosted PDC: a test CLAP reporting 256 samples of latency is aligned to the sample with a dry track: `cargo test -p ow-plugin-host clap_pdc`.
  - Hosted automation: an automation ramp on a hosted parameter arrives as sample-accurate parameter events: `cargo test -p ow-plugin-host clap_automation`.
  - Main thread: work posted through `MainThreadExecutor` runs on the designated thread, and the host's `is_main_thread` is true there and false on audio and worker threads: `cargo test -p ow-plugin-host --test main_thread` (all three OSes).
  - Entitlement present: in the macOS build job, `codesign -d --entitlements - "OpenWave Studio.app"` on the ad-hoc signed bundle shows `com.apple.security.cs.disable-library-validation`.
- Status: pending
- Outcome:
- Handoff notes: use clack's example plugins as fixtures until stage 27.

## Stage 25: Plugin hosting II: scanner, browser and editor windows

- Objective: out-of-process scanner (`--scan-plugin`, timeout, blacklist, `plugin-cache.json`); plugin browser; `ow-plugin-host::editor` creating plain platform windows (Win32, AppKit, X11 via `x11rb`) and passing raw handles; the Linux editor run loop, built on stage 24's `MainThreadExecutor` (it becomes the Linux plugin main thread), servicing the X11 fd plus CLAP `posix-fd-support` and `timer-support`; a purpose-built test CLAP (`crates/ow-plugin-host/tests/plugins/runloop_clap/`) that registers a pipe fd and a 10 ms timer and records which thread its callbacks ran on; `docs/plugins.md` (hosting section).
- Depends on: stage 24
- Owned paths: `crates/ow-plugin-host/src/scan/`, `crates/ow-plugin-host/src/editor/`, `crates/ow-plugin-host/tests/scan.rs`, `crates/ow-plugin-host/tests/clap_runloop.rs`, `crates/ow-plugin-host/tests/plugins/runloop_clap/`, `src-tauri/src/plugin_windows.rs`, `src/lib/plugins/`, `docs/plugins.md` (hosting section)
- Acceptance checks:
  - A crashing test plugin is blacklisted without crashing the app: `cargo test -p ow-plugin-host scan_crash`.
  - Run loop: the `runloop_clap` test plugin receives its fd callback after a write to the pipe and at least 9 timer callbacks in 100 ms, and the host's `is_main_thread` is true inside every gui, timer and fd callback: `cargo test -p ow-plugin-host --test clap_runloop` (Linux CI with Xvfb).
  - Editors (manual, per OS, Linux under both X11 and Wayland sessions): a third-party CLAP (for example Surge XT) opens its editor within 3 s, responds to input and closes cleanly. Record in Outcome.
- Status: pending
- Outcome:
- Handoff notes: Tauri's `unstable` window feature is deliberately not used (architecture record section 8).

## Stage 26: Plugin hosting III: VST3

- Objective: VST3 hosting through the `vst3` crate: module loading and factory, single and split component/controller, parameter changes and event lists (automation), state, latency (PDC), bus arrangements, `IPlugFrame` editors in stage 25's windows, `IRunLoop` on Linux on stage 25's run loop, scan paths per OS.
- Depends on: stage 25
- Owned paths: `crates/ow-plugin-host/src/vst3/`, `crates/ow-plugin-host/tests/vst3.rs`, `crates/ow-plugin-host/tests/vst3_runloop.rs`, `crates/ow-plugin-host/tests/plugins/runloop_vst3/` (purpose-built test VST3 registering an `IRunLoop` event handler and timer)
- Acceptance checks:
  - Processing, state, automation and PDC on all three OSes: `cargo test -p ow-plugin-host vst3` (CI, against the SDK's example plugins built in CI).
  - Linux `IRunLoop`: the `runloop_vst3` test plugin's event handler fires after a pipe write and its 10 ms timer fires at least 9 times in 100 ms, on the plugin main thread: `cargo test -p ow-plugin-host --test vst3_runloop` (Xvfb).
  - A third-party VST3 opens its editor within 3 s, plays, and restores state after reload (manual, per OS). Record in Outcome.
- Status: pending
- Outcome:
- Handoff notes: Minimum accepted slice: processing, parameters, state and PDC with tests passing; editors and `IRunLoop` may follow in a second session. Switch fixtures to OpenWave's own VST3s after stage 27.

## Stage 27: Plugin export: CLAP and VST3

- Objective: `plugins/ow-plugins` exporting every built-in instrument and effect through nice-plug (`vst3` feature on the MIT `vst3` crate) as `.clap` and `.vst3`; `nice-plug-egui` editors generated from descriptors; `xtask bundle-plugins`; CI validator job (`clap-validator`, VST3 `validator`); `build.yml` packages `OpenWave-Plugins-<version>-<platform>.zip` (macOS bundles ad-hoc signed) and both `publish-release.sh` and `publish-experimental.sh` attach it; `docs/plugins.md` export section, including the note that unnotarized macOS plugin bundles may need `xattr -dr com.apple.quarantine` before a DAW loads them.
- Depends on: stages 20, 26
- Owned paths: `plugins/ow-plugins/`, `xtask/`, `.github/workflows/ci.yml` (validator job), `.github/workflows/build.yml` (plugin packaging), `scripts/ci/bundle-plugins.sh`, `scripts/ci/publish-release.sh` and `publish-experimental.sh` (plugin zip attach lines), `docs/plugins.md` (export section)
- Acceptance checks:
  - No GPL VST3 path: `cargo tree -p ow-plugins -i vst3-sys` finds nothing and `cargo deny check licenses` passes.
  - Validators pass for every exported plugin on all three OSes (CI job green).
  - Both channels carry plugins: `gh release view vX.Y.N-experimental --json assets` and `gh release view <stable tag> --json assets` each list `OpenWave-Plugins-*` zips for every platform.
  - In-app device and exported plugin render identically: `cargo test -p ow-plugins parity`.
  - The exported synth loads and plays in OpenWave and in one third-party DAW (for example REAPER) (manual). Record in Outcome.
- Status: pending
- Outcome:
- Handoff notes: Minimum accepted slice: synth, sampler, EQ, compressor and reverb exported and validated; the rest follow under the same stage. Use "VST3" as text only; no VST logo; check Steinberg's trademark guidelines before the first release with plugins.

## Stage 28: Plugin export: AUv2 on macOS

- Objective: `plugins/au-wrapper` CMake project using clap-wrapper to build `.component` bundles from the exported `.clap` files on the macOS runner, included in the macOS plugin zip; bundles ad-hoc signed; `auval` CI job; license notes for clap-wrapper (MIT) and AudioUnitSDK (Apache-2.0) shipped in the zip and recorded in `docs/plugins.md`.
- Depends on: stage 27
- Owned paths: `plugins/au-wrapper/`, `scripts/ci/bundle-au.sh`, `.github/workflows/build.yml` (macOS AU step), `.github/workflows/ci.yml` (auval job), `docs/plugins.md` (AU and license section)
- Acceptance checks:
  - `auval -a` lists the OpenWave components and `auval -v` passes for each (macOS CI).
  - License notices present in the macOS plugin zip (manual check of the zip listing, recorded in Outcome).
  - The exported synth loads in GarageBand or Logic (manual). Record in Outcome.
- Status: pending
- Outcome:
- Handoff notes: if clap-wrapper cannot build on the runner within one session, mark blocked with the reason; macOS keeps CLAP and VST3 (architecture revisit condition).

## Stage 29: Plugin hosting IV: Audio Units on macOS

- Objective: AU (v2 and v3) hosting with `objc2-audio-toolbox`/`objc2-avf-audio`: `AUAudioUnit` instantiation, processing through its render block with our pull-input block, parameter tree (automation), full state, latency (PDC), view controllers in stage 25's AppKit windows.
- Depends on: stage 25
- Owned paths: `crates/ow-plugin-host/src/au/`, `crates/ow-plugin-host/tests/au.rs`
- Acceptance checks:
  - Processing, parameters, state and latency with Apple's built-in AUs (for example AUDelay): `cargo test -p ow-plugin-host au` (macOS CI).
  - Apple's AUSampler opens its view and plays (manual). Record in Outcome.
- Status: pending
- Outcome:
- Handoff notes: AUv3 out-of-process units render through the same block API.

## Stage 30: Plugin hosting V: LV2 on Linux

- Objective: LV2 hosting through `livi` over `lilv` (bundled into the AppImage), atoms for MIDI, parameters (automation), state, latency (PDC), generic UI (native X11 UIs where available); license notes for lilv/serd/sord/sratom (ISC) in the AppImage and `docs/plugins.md`.
- Depends on: stage 25
- Owned paths: `crates/ow-plugin-host/src/lv2/`, `crates/ow-plugin-host/tests/lv2.rs`, `scripts/ci/install-linux-deps.sh` (lilv), `.github/workflows/build.yml` (AppImage libs), `src-tauri/tauri.conf.json` (`bundle.linux.appimage`), `docs/plugins.md` (LV2 and license section)
- Acceptance checks:
  - `cargo test -p ow-plugin-host lv2` passes on Linux CI with a packaged LV2 test plugin.
  - The AppImage runs LV2 hosting on a machine without lilv installed (manual). Record in Outcome.
  - License notices present in the AppImage (manual check, recorded in Outcome).
- Status: pending
- Outcome:
- Handoff notes: LV2 GUI support may stay generic in this stage.
