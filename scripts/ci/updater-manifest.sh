#!/usr/bin/env bash
# Write the updater feed, latest.json, from the signed installers in release-dist/:
#   updater-manifest.sh <version> <tag> [output-file]
# The output defaults to `latest.json` in the working directory (not inside release-dist/, so it
# never joins the versioned release). Fails unless
# every platform key, installer and portable, is present; a partial feed would strand some platforms.
# Needs GITHUB_REPOSITORY and jq.
set -euo pipefail

version="$1" tag="$2" out="${3:-latest.json}"
dir="${RELEASE_DIST:-release-dist}"
base="https://github.com/${GITHUB_REPOSITORY:?}/releases/download/$tag"

# Updater platform keys and the installer file-name suffix each one downloads. The plain key
# (for example windows-x86_64) is the fallback the plugin tries last; the installer-specific key
# (-nsis, -appimage) is tried first, so a future second installer type cannot be confused. The
# -portable keys are not installers: the app asks for them by name (custom target) to update a
# Windows portable folder from the signed portable zip.
platforms=(
  "windows-x86_64:_x64-setup.exe"
  "windows-x86_64-nsis:_x64-setup.exe"
  "windows-aarch64:_arm64-setup.exe"
  "windows-aarch64-nsis:_arm64-setup.exe"
  "windows-x86_64-portable:_x64_portable.zip"
  "windows-aarch64-portable:_arm64_portable.zip"
  "darwin-x86_64:_universal.app.tar.gz"
  "darwin-aarch64:_universal.app.tar.gz"
  "linux-x86_64:_amd64.AppImage"
  "linux-x86_64-appimage:_amd64.AppImage"
  "linux-aarch64:_aarch64.AppImage"
  "linux-aarch64-appimage:_aarch64.AppImage"
)

entries='{}'
missing=()
for platform in "${platforms[@]}"; do
  key="${platform%%:*}" suffix="${platform#*:}"
  sig=""
  for candidate in "$dir"/*"$suffix".sig; do
    if [ -e "$candidate" ]; then sig="$candidate"; break; fi
  done
  if [ -z "$sig" ]; then
    missing+=("$key")
    continue
  fi
  file="$(basename "${sig%.sig}")"
  entries="$(jq --arg key "$key" --arg url "$base/$file" --rawfile signature "$sig" \
    '.[$key] = {url: $url, signature: ($signature | rtrimstr("\n"))}' <<< "$entries")"
done

if [ "${#missing[@]}" -gt 0 ]; then
  echo "::error::No signed installer for updater platform(s): ${missing[*]}"
  exit 1
fi

jq -n --arg version "$version" --arg notes "OpenWave Studio $tag" \
  --arg date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson platforms "$entries" \
  '{version: $version, notes: $notes, pub_date: $date, platforms: $platforms}' > "$out"
jq -r '.platforms | keys | join(", ") | "Updater feed platforms: " + .' "$out"
