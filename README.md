# OpenWave Studio

A free, open source, portable digital audio workstation (DAW).

OpenWave Studio is built on [Tauri](https://tauri.app/): a Rust core handles audio processing and system access with native performance, while a lightweight web-based UI runs on the platform's own webview. The result is a small, fast application that runs on Windows, macOS and Linux.

## Goals

- **Free and open source** — no paywalls, no subscriptions, no telemetry.
- **Portable** — runs without installation from a single folder or drive; settings and projects travel with it.
- **Native performance** — real-time audio engine written in Rust, low-latency I/O on every platform.
- **Cross-platform** — one codebase for Windows, macOS and Linux.

## Status

Early development. Nothing is usable yet.

## Install

Download the latest build from the [releases page](https://github.com/Densetsu152637/OpenWave-Studio/releases). Every release lists `SHA256SUMS` for checking the downloads.

| Platform | Installer | Portable |
| --- | --- | --- |
| Windows (x64, arm64) | `OpenWave-Studio_<version>_<arch>-setup.exe` | `OpenWave-Studio_<version>_<arch>_portable.zip` |
| macOS (Apple silicon and Intel) | `OpenWave-Studio_<version>_universal.dmg` | `OpenWave-Studio_<version>_universal_portable.zip` |
| Linux (x64, arm64) | `.deb` and `.rpm` packages | `.AppImage` |

Experimental builds from the `experimental` branch are published as pre-releases named `vX.Y.N-experimental`; they may be unstable.

### macOS first launch

The macOS app is ad-hoc signed but not notarized, because there is no Apple Developer ID yet. Gatekeeper therefore blocks the first launch of a downloaded copy. Clear it once per manual download, either way:

- Right-click (or Control-click) `OpenWave Studio.app` and choose Open, then confirm. On newer macOS versions, choose Open Anyway in System Settings > Privacy & Security after the first attempt.
- Or remove the quarantine flag in a terminal: `xattr -dr com.apple.quarantine "/path/to/OpenWave Studio.app"`.

## Building from source

Requirements: stable Rust (installed by `rust-toolchain.toml`), Node 24 and pnpm 10. On Linux, install the system libraries with `bash scripts/ci/install-linux-deps.sh` (Debian/Ubuntu).

```sh
pnpm install
pnpm tauri dev          # run the app with hot reload
pnpm tauri build --no-bundle   # unbundled release binary in target/release/
bash scripts/check-web.sh      # typecheck, unit tests and build of the web UI
bash scripts/check-rust.sh     # fmt, clippy and tests (needs `pnpm build` first)
```

Portable mode: place an empty file named `portable` beside the executable (beside the AppImage on Linux, beside the `.app` bundle on macOS) and all settings, caches, logs and projects are kept in an `OpenWave Studio Data` folder next to it.

`openwave-studio --version` prints the version and exits without opening a window.

## Contributing

Issues and pull requests are welcome.

## License

[MIT](LICENSE)
