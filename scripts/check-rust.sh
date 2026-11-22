#!/usr/bin/env bash
# Rust checks, shared by local development and CI. Needs the frontend build output
# (`pnpm install && pnpm build`) because the app crate embeds `dist/`; Linux also needs
# `scripts/ci/install-linux-deps.sh`.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
