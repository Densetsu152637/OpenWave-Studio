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
