# Releasing OpenWave Studio

How builds are produced and published. Design rationale lives in the [architecture record](architecture.md#10-release-ci-update-channels-and-portable-builds); this page is the operating procedure.

## Channels and tags

| Channel | Trigger | Tag and version | GitHub Release |
| --- | --- | --- | --- |
| main (stable) | push a tag `vX.Y.Z` on a commit that is on `main` | tag `vX.Y.Z`, version `X.Y.Z` | marked latest |
| main (pre-release) | push a tag `vX.Y.Z-suffix` (any hyphen) on a `main` commit | tag `vX.Y.Z-suffix` | pre-release, not latest |
| experimental | push to the `experimental` branch | tag `vX.Y.N-experimental`, where `X.Y` comes from `src-tauri/tauri.conf.json` and `N` is the workflow run number | pre-release, not latest |

Workflows: `ci.yml` checks pull requests and pushes; `release.yml` and `experimental.yml` run `prepare`, then `build.yml` (a reusable workflow with the inputs `channel` and `version`), then `publish`. Tags matching `v*-experimental*` never start `release.yml`.

Each channel keeps one release: after publishing, the channel's older releases are deleted together with their tags. Stable releases replace older stable and pre-release main releases; main pre-releases replace only older main pre-releases. Only older releases are ever deleted.

Re-running an old workflow cannot roll a channel back: before creating a release, the publish script exits successfully with a notice if the channel already has that tag or a newer version (experimental: the run-number patch; main: semantic version, where a release beats its own pre-releases; releases of the other channel are ignored). Release workflows share one concurrency group, `release` for tags and `experimental` for the branch, and never cancel a running build.

## Branch rules

- Work lands on `experimental`. A pull request into `main` must come from `experimental` in this repository; the `source branch` check in `ci.yml` fails otherwise.
- Ancestry rule: `prepare` checks out the full history and fails unless the built commit is an ancestor of `origin/main` (release tags) or `origin/experimental` (experimental builds). A tag on a commit that is not on `main` therefore never produces a release.
- To ship a stable release: merge `experimental` into `main` through a pull request, wait for `CI gate`, then tag the merge commit (`git tag vX.Y.Z <sha>; git push origin vX.Y.Z`). Bump `version` in `Cargo.toml`, `package.json` and `src-tauri/tauri.conf.json` first so experimental versions (`X.Y.N`) follow the app version.

## Build targets and assets

`build.yml` runs a five-target matrix: `linux-x64` and `linux-arm64` (AppImage, deb, rpm), `windows-x64` and `windows-arm64` (NSIS installer only; there is no MSI), and `macos-universal` (app, dmg). RPM is skipped for hyphenated versions because RPM rejects the hyphen. Builds use `tauri-apps/tauri-action` in build-only mode (no `tagName` or `releaseId`, so it uploads nothing) pinned to the commit of its `action-v1.0.0` tag.

Every release carries, per target, the installers plus a portable package, and one `SHA256SUMS` file:

- Windows: `OpenWave-Studio_<version>_<arch>-setup.exe` and `OpenWave-Studio_<version>_<arch>_portable.zip` (the plain executable, a `portable` marker file and the license).
- macOS: `OpenWave-Studio_<version>_universal.dmg` and `OpenWave-Studio_<version>_universal_portable.zip` (the `.app` bundle, a `portable` marker beside it and the license).
- Linux: the `.AppImage` is the portable artifact (place a `portable` marker beside it), plus `.deb` and `.rpm`.

`scripts/ci/collect-dist.sh` gathers these into `release-dist/` (asset names contain hyphens, not spaces), checks that each portable zip holds the program, `LICENSE` and the `portable` marker at its top level, and requires the `.rpm` files for versions without a hyphen, and `scripts/ci/smoke.sh` runs each built target's `--version` before anything is published. Updater artifacts are off (`createUpdaterArtifacts` is `false`) until the updater stage adds a signing key, so no `latest.json` feed is written yet.

## Required secrets

None today. The updater stage adds `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Apple secrets (`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`) are optional: when present, `build.yml` signs with that Developer ID and notarizes; when absent, nothing else changes. `release.yml` and `experimental.yml` pass these secrets to `build.yml` explicitly (no `secrets: inherit`), and `build.yml` hands them only to the tauri-action step, through `scripts/ci/tauri.sh`, which drops the empty ones. The publish jobs use only the workflow's `GITHUB_TOKEN` (`contents: write`).

## macOS signing and Gatekeeper

Builds are ad-hoc signed (`bundle.macOS.signingIdentity` is `"-"` in `src-tauri/tauri.conf.json`) and not notarized. The configuration keys, verified against the Tauri 2.12 config schema, are `bundle.macOS.signingIdentity`, `bundle.macOS.entitlements` (path to `src-tauri/entitlements.plist`) and `bundle.macOS.infoPlist` (path to `src-tauri/Info.plist`, merged into the bundle's `Info.plist`; it carries `NSMicrophoneUsageDescription`). `bundle.macOS.hardenedRuntime` exists and defaults to `true`, and Tauri applies it to executables whenever it signs, ad-hoc included. So the hardened runtime is active on our builds, and the entitlements file matters from the first release: it embeds `com.apple.security.device.audio-input` for microphone capture.

The build job asserts `codesign -dv` reports `Signature=adhoc`, that the signature verifies, that the audio-input entitlement is embedded, and that the signed app's executable exits 0 for `--version`.

Because the app is not notarized, Gatekeeper blocks the first launch of a downloaded copy. Either right-click `OpenWave Studio.app` and choose Open (on newer macOS: Open Anyway in System Settings > Privacy & Security), or run `xattr -dr com.apple.quarantine "/path/to/OpenWave Studio.app"`. Updates installed by the updater are not quarantined, so this is needed once per manual download.

Ad-hoc signing binds the macOS privacy (TCC) microphone grant to the code-directory hash, so each new build may ask for microphone access again.

## Verifying a download

Every release lists `SHA256SUMS`, which covers the installers, portable packages and signatures published with that release. The update feed `latest.json` (from the updater stage) lives in the `updater-<channel>` releases and is not in `SHA256SUMS`. Download it next to the files you want and check them (`sha256sum -c --ignore-missing SHA256SUMS` on Linux, `shasum -a 256 -c --ignore-missing SHA256SUMS` on macOS, or compare `Get-FileHash <file>` with the matching line on Windows).

## Forcing the update feed

The update feed (stage 3) normally refuses to move backwards: a publish whose version is lower than the current feed's is skipped. For an intentional version reset, such as after a mistakenly high version, dispatch the workflow manually with `force_feed` set to `true`: `experimental.yml` from the `experimental` branch, or `release.yml` on an existing tag ref. The input counts only when it is exactly `true`. The override applies to that run only: it skips the newer-feed check and also the newer-release check above (so a lower version can be published; the newer release is not deleted and must be removed by hand), and it never bypasses the branch-ancestry check. The input and the `FORCE_FEED` variable are already wired through the publish scripts; the feed itself arrives with the updater stage.

## Local checks

```sh
actionlint                          # workflow syntax
cargo deny check licenses           # dependency licenses (deny.toml); GPL, LGPL and AGPL are rejected
npx markdownlint-cli2 "**/*.md"     # documentation lint
python3 scripts/ci/check-links.py . # relative links
```
