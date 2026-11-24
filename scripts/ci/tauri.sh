#!/usr/bin/env bash
# `tauri` wrapper for tauri-action (tauriScript). Tauri treats an empty signing variable as
# configured, and a workflow cannot leave a secret unset, so drop the empty ones here and then
# run the Tauri CLI with the arguments tauri-action passes.
set -euo pipefail

for var in TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD \
  APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY \
  APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
  if [ -z "${!var:-}" ]; then unset "$var"; fi
done

exec pnpm tauri "$@"
