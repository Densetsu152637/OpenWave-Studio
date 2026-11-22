#!/usr/bin/env bash
# Launch smoke test of one built target: smoke.sh <platform-id> <tauri-target> <version>
# macOS: the signed .app must be ad-hoc signed (unless a Developer ID identity was configured),
#        verify as a valid signature, and `--version` on its executable must exit 0.
# Windows and Linux: the release executable's `--version` must print the build's version.
set -euo pipefail

id="$1" target="$2" version="$3"
release_dir="target/$target/release"

case "$id" in
  macos-*)
    app="$release_dir/bundle/macos/OpenWave Studio.app"
    codesign -dvv "$app" 2>&1 | tee codesign.txt
    if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
      grep -q 'Signature=adhoc' codesign.txt
    fi
    codesign --verify --deep --strict --verbose=2 "$app"
    codesign -d --entitlements - "$app" 2>&1 | tee entitlements.txt
    grep -q 'com.apple.security.device.audio-input' entitlements.txt
    rm -f codesign.txt entitlements.txt
    exe_name="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist")"
    # Exit status only: a signing or entitlement error kills the process at load.
    "$app/Contents/MacOS/$exe_name" --version
    ;;
  *)
    exe="$release_dir/openwave-studio"
    if [ "$id" = "${id#linux-}" ]; then exe="$exe.exe"; fi
    actual="$("$exe" --version)"
    echo "expected=[$version] actual=[$actual]"
    test "$actual" = "$version"
    ;;
esac
