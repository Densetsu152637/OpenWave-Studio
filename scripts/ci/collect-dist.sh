#!/usr/bin/env bash
# Gather one matrix target's release files into release-dist/: collect-dist.sh <platform-id> <tauri-target> <version>
# Environment:
#   ARTIFACT_PATHS  JSON array of bundle paths, tauri-action's `artifactPaths` output
#   UPDATER         "true" when updater artifacts were requested; then every installer needs a .sig
# Copies the installers (spaces in names become hyphens, which is what GitHub stores), then builds
# the portable package: a plain-exe zip on Windows, the .app zip on macOS (each with a `portable`
# marker beside the program), and on Linux the AppImage itself.
set -euo pipefail

id="$1" target="$2" version="$3"
updater="${UPDATER:-false}"
release_dir="target/$target/release"
mkdir -p release-dist

# Installers and updater files reported by tauri-action.
if [ -z "${ARTIFACT_PATHS:-}" ]; then
  echo "::error::tauri-action reported no artifacts."
  exit 1
fi
while IFS= read -r path; do
  [ -n "$path" ] || continue
  case "$path" in
    *.AppImage|*.deb|*.rpm|*.dmg|*-setup.exe|*.sig|*.app.tar.gz) ;;
    *) continue ;;
  esac
  name="$(basename "$path")"
  cp "$path" "release-dist/${name// /-}"
done < <(node -p 'JSON.parse(process.env.ARTIFACT_PATHS).map((p) => p.split(String.fromCharCode(92)).join("/")).join(String.fromCharCode(10))')

# The macOS update archive is named after the product only; give it the version and target.
for archive in release-dist/*.app.tar.gz; do
  [ -e "$archive" ] || continue
  renamed="release-dist/OpenWave-Studio_${version}_${id#macos-}.app.tar.gz"
  mv "$archive" "$renamed"
  if [ -e "$archive.sig" ]; then mv "$archive.sig" "$renamed.sig"; fi
done

portable=""
case "$id" in
  windows-*)
    stage="$(mktemp -d)"
    cp "$release_dir/openwave-studio.exe" "$stage/OpenWave Studio.exe"
    cp LICENSE "$stage/LICENSE"
    : > "$stage/portable"
    portable="OpenWave-Studio_${version}_${id#windows-}_portable.zip"
    out="$(cygpath -w "$PWD/release-dist/$portable")"
    powershell.exe -NoProfile -Command "Compress-Archive -Path '$(cygpath -w "$stage")\*' -DestinationPath '$out'"
    ;;
  macos-*)
    stage="$(mktemp -d)"
    ditto "$release_dir/bundle/macos/OpenWave Studio.app" "$stage/OpenWave Studio.app"
    cp LICENSE "$stage/LICENSE"
    : > "$stage/portable"
    portable="OpenWave-Studio_${version}_${id#macos-}_portable.zip"
    ditto -c -k --norsrc "$stage" "release-dist/$portable"
    ;;
  linux-*) portable="" ;; # the AppImage is the portable artifact
  *) echo "::error::unknown platform id $id"; exit 1 ;;
esac

# Every installer that updates itself needs its signature; the portable zips are signed from
# the portable-update stage on.
if [ "$updater" = true ]; then
  for file in release-dist/*-setup.exe release-dist/*.app.tar.gz release-dist/*.AppImage; do
    [ -e "$file" ] || continue
    if [ ! -e "$file.sig" ]; then
      echo "::error::$file has no updater signature (.sig)."
      exit 1
    fi
  done
fi

# The expected files must exist.
need=()
case "$id" in
  windows-*) need=("release-dist/*-setup.exe" "release-dist/$portable") ;;
  macos-*) need=("release-dist/*.dmg" "release-dist/$portable") ;;
  linux-*) need=("release-dist/*.AppImage" "release-dist/*.deb") ;;
esac
for pattern in "${need[@]}"; do
  # shellcheck disable=SC2086
  if ! ls $pattern > /dev/null 2>&1; then
    echo "::error::Missing expected file $pattern in release-dist/."
    exit 1
  fi
done
ls -l release-dist
