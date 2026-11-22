#!/usr/bin/env bash
# System libraries needed to build and bundle OpenWave Studio on Ubuntu: Tauri's WebKitGTK stack,
# the ALSA and JACK development packages that cpal links against, and the AppImage/RPM bundling
# tools. Later stages extend this file.
set -euo pipefail

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential curl file patchelf pkg-config libssl-dev libxdo-dev \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev \
  libasound2-dev libjack-jackd2-dev \
  libfuse2 rpm xdg-utils
