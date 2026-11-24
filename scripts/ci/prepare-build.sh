#!/usr/bin/env bash
# Prepare one matrix target's build: prepare-build.sh <tauri-target> <bundles> <version>
# Writes target/ow-build-config.json (the `tauri build --config` overlay: channel version, updater
# artifacts) and reports `args`, the tauri-action argument string, and `updater` as step outputs.
set -euo pipefail

target="$1" bundles="$2" version="$3"
out="${GITHUB_OUTPUT:-/dev/stdout}"

# RPM rejects the hyphen in pre-release versions; MSI is never built (NSIS only on Windows).
if [[ "$version" == *-* ]]; then
  bundles="$(tr ',' '\n' <<< "$bundles" | grep -vxE 'rpm' | paste -sd, -)"
fi

# Signed updater artifacts (.sig beside each installer); they need the TAURI_SIGNING_PRIVATE_KEY
# secret, which reaches only the tauri-action step. The feed is built from them in publish.
updater=true

mkdir -p target
printf '{"version":"%s","bundle":{"createUpdaterArtifacts":%s}}\n' "$version" "$updater" \
  > target/ow-build-config.json

{
  echo "args=--target $target --bundles $bundles --config target/ow-build-config.json -- --locked"
  echo "updater=$updater"
} >> "$out"
