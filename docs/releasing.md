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

`scripts/ci/collect-dist.sh` gathers these into `release-dist/` (asset names contain hyphens, not spaces), checks that each portable zip holds the program, `LICENSE` and the `portable` marker at its top level, and requires the `.rpm` files for versions without a hyphen, and `scripts/ci/smoke.sh` runs each built target's `--version` before anything is published. `scripts/ci/prepare-build.sh` turns `bundle.createUpdaterArtifacts` on through the `--config` overlay, so every NSIS installer, `.app.tar.gz` and AppImage is built with a `.sig` beside it, and `collect-dist.sh` fails if one is missing. The signatures use the repository secrets below. The portable zips are signed by `collect-dist.sh` itself (see [Portable builds](#portable-builds)).

## Required secrets

`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` sign the updater artifacts; see [Update feeds and signing keys](#update-feeds-and-signing-keys). Apple secrets (`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`) are optional: when present, `build.yml` signs with that Developer ID and notarizes; when absent, nothing else changes. `release.yml` and `experimental.yml` pass these secrets to `build.yml` explicitly (no `secrets: inherit`), and `build.yml` hands them only to the tauri-action step, through `scripts/ci/tauri.sh`, which drops the empty ones. The publish jobs use only the workflow's `GITHUB_TOKEN` (`contents: write`).

## macOS signing and Gatekeeper

Builds are ad-hoc signed (`bundle.macOS.signingIdentity` is `"-"` in `src-tauri/tauri.conf.json`) and not notarized. The configuration keys, verified against the Tauri 2.12 config schema, are `bundle.macOS.signingIdentity`, `bundle.macOS.entitlements` (path to `src-tauri/entitlements.plist`) and `bundle.macOS.infoPlist` (path to `src-tauri/Info.plist`, merged into the bundle's `Info.plist`; it carries `NSMicrophoneUsageDescription`). `bundle.macOS.hardenedRuntime` exists and defaults to `true`, and Tauri applies it to executables whenever it signs, ad-hoc included. So the hardened runtime is active on our builds, and the entitlements file matters from the first release: it embeds `com.apple.security.device.audio-input` for microphone capture.

The build job asserts `codesign -dv` reports `Signature=adhoc`, that the signature verifies, that the audio-input entitlement is embedded, and that the signed app's executable exits 0 for `--version`.

Because the app is not notarized, Gatekeeper blocks the first launch of a downloaded copy. Either right-click `OpenWave Studio.app` and choose Open (on newer macOS: Open Anyway in System Settings > Privacy & Security), or run `xattr -dr com.apple.quarantine "/path/to/OpenWave Studio.app"`. Updates installed by the updater are not quarantined, so this is needed once per manual download.

Ad-hoc signing binds the macOS privacy (TCC) microphone grant to the code-directory hash, so each new build may ask for microphone access again. Unverified: whether an update installed over an allowed build really prompts again has not been observed on a Mac; the stage 14 manual check (first record press on a clean Mac, then a later update) records it.

## Verifying a download

Every release lists `SHA256SUMS`, which covers the installers, portable packages and signatures published with that release. The update feed `latest.json` lives in the `updater-<channel>` releases and is not in `SHA256SUMS`. Download it next to the files you want and check them (`sha256sum -c --ignore-missing SHA256SUMS` on Linux, `shasum -a 256 -c --ignore-missing SHA256SUMS` on macOS, or compare `Get-FileHash <file>` with the matching line on Windows).

## Update feeds and signing keys

Each channel has a feed: the asset `latest.json` of a fixed release, `updater-main` or `updater-experimental` (the tags start with `updater-`, so they never match the `v*` trigger and the prune scripts skip them). The app reads `https://github.com/Densetsu152637/OpenWave-Studio/releases/download/updater-<channel>/latest.json`. Both releases are pre-releases that are never marked latest, and are created by CI when absent.

Publish order, in `scripts/ci/publish-release.sh` and `publish-experimental.sh`: (1) create the versioned release with all assets, (2) `scripts/ci/publish-feed.sh` writes the feed, (3) prune older releases. So the feed never points at a missing asset. The feed step:

1. Downloads the current `latest.json` and skips the upload, with a notice, when its version is greater than the one being published (a late older run cannot roll the channel back).
2. Runs `scripts/ci/updater-manifest.sh`, which builds `latest.json` from the `.sig` files in `release-dist/` and fails unless all twelve platform keys are present, the ten installer keys and the two portable keys (below): `windows-x86_64`, `windows-x86_64-nsis`, `windows-aarch64`, `windows-aarch64-nsis`, `darwin-x86_64`, `darwin-aarch64`, `linux-x86_64`, `linux-x86_64-appimage`, `linux-aarch64`, `linux-aarch64-appimage`. The two macOS keys point at the same universal `.app.tar.gz`. The two `windows-x86_64-portable` and `windows-aarch64-portable` keys point at the signed portable zips; only the app's Windows portable path asks for them by name (installed builds never select them).
3. Runs `scripts/ci/check-feed.sh`, which requests every URL in the new feed (redirects followed, with retries) and fails unless each answers HTTP 200.
4. Uploads it with `gh release upload --clobber`.

The publish job of each workflow holds `concurrency: { group: updater-feed-<channel>, cancel-in-progress: false }`, so two runs never write one feed at once. Only stable main releases write the main feed: a main pre-release (for example `v0.2.0-rc.1`) never feeds the stable channel.

GitHub serves release assets through a cache: after the feed is replaced, the public URL can keep returning the previous `latest.json` for about three minutes (observed), while `gh release download updater-<channel> -p latest.json -O -` shows the new one at once. Wait that long after a publish before testing an update, and expect installed apps to see a new build that much later.

Check a feed from outside: `curl -sL <feed url> | jq -r '.platforms | keys[]'` and `scripts/ci/check-feed.sh <feed url>`.

### Signing keys

The updater verifies every download against the public key in `src-tauri/tauri.conf.json` (`plugins.updater.pubkey`). The matching private key and its password are the repository secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; the public key is also kept as the secret `TAURI_SIGNING_PUBLIC_KEY` for reference. Generate a pair with `pnpm tauri signer generate`. The private key must never be committed or printed. Rotating the key strands installed builds: they accept only updates signed by the key they carry, so a rotation needs a release signed with the old key that carries the new public key first.

### Forcing the update feed

The feed normally refuses to move backwards. For an intentional version reset, such as after a mistakenly high version, dispatch the workflow manually with `force_feed` set to `true`: `experimental.yml` from the `experimental` branch, or `release.yml` on an existing tag ref (`prepare` fails on a branch ref). The input counts only when it is exactly `true`. The override applies to that run only: it skips the newer-release check and the feed's newer-version check, so a lower version can be published and written to the feed (the newer release is not deleted and must be removed by hand), and it never bypasses the branch-ancestry check. Afterwards restore the feed with a normal publish of the newest build.

Note that a fresh dispatch always gets a new, higher run number. To publish an older build on purpose, re-run the publish job of an older run that was itself started with `force_feed=true` (a re-run keeps its original inputs).

## Update behavior in the app

The channel defaults to the one the running version was released on (a version containing `-experimental` follows experimental, anything else main) and can be switched in Settings. Checks run 20 s after start and every 6 h; a newer build downloads in the background and installs when the app quits, or at once with "Restart to update" in the menu bar. The menu bar also shows the installed version (tagged "experimental" on that channel), which opens Settings › About. There, the Updates section has the channel choice (Main: stable releases; Experimental: every build of the experimental branch, may be unstable) and "Check and install updates", which checks now and, when a build is found, downloads it, installs it and restarts. After a channel switch exactly one check accepts any different version (so experimental can move back to the current stable); it is not saved. Linux installs without `$APPIMAGE` (deb, rpm) are package-managed: no checks, no feed requests, and the panel says "Update OpenWave Studio through your package manager". Portable builds update themselves as described in [Portable builds](#portable-builds).

## Portable builds

`collect-dist.sh` builds the portable packages and signs them in the build job: the Windows zip (plain executable, `portable` marker, license) and the macOS zip (`.app`, marker, license) each get a `<zip>.sig` from `tauri signer sign`, using `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, which `build.yml` passes to the "Collect release files" step only, and with `--app-version <version>`, which writes the version into the signature's trusted comment. The script then fails the job if any updater artifact (NSIS installer, `.app.tar.gz`, AppImage) or any portable zip lacks a non-empty `.sig`. A release therefore shows a `.sig` beside every portable zip: `gh release view <tag> --json assets --jq '.assets[].name' | grep portable`.

The Windows portable zips feed the self-update path (`src-tauri/src/portable_update.rs`); the macOS zip and the Linux AppImage update through the standard updater, so their `.sig` files are only for verification. How the app swaps the files, the 30 s retry budget and the macOS quarantine and translocation workaround are in [portable.md](portable.md).

The app sets `plugins.updater.requireSignedVersion` in `src-tauri/tauri.conf.json`: the updater plugin rejects an update whose announced version differs from the version signed into its signature, so a tampered feed cannot pair a new version number with an older release. Every signature in a feed must therefore carry `version:`. The Tauri bundler writes it for installers and the AppImage/macOS archive, `collect-dist.sh` does for the portable zips, and `updater-manifest.sh` refuses to write a feed in which any signature's version differs from the feed version. Check a live feed: decode each signature (`base64 -d`) and look for `version:<feed version>` in its `trusted comment` line. Builds before this setting do not enforce it; only builds that carry it do.

Check a published feed for the portable keys: `curl -sL <feed url> | jq -r '.platforms | keys[]' | grep portable` lists `windows-aarch64-portable` and `windows-x86_64-portable`.

## Local checks

```sh
actionlint                          # workflow syntax
cargo deny check licenses           # dependency licenses (deny.toml); GPL, LGPL and AGPL are rejected
npx markdownlint-cli2 "**/*.md"     # documentation lint
python3 scripts/ci/check-links.py . # relative links
```
