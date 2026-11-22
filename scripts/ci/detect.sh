#!/usr/bin/env bash
# Report which parts of the repository exist, as GitHub Actions step outputs.
set -euo pipefail

out="${GITHUB_OUTPUT:-/dev/stdout}"
has() { if [ -e "$1" ]; then echo true; else echo false; fi; }
{
  echo "rust=$(has Cargo.toml)"
  echo "web=$(has package.json)"
  echo "desktop=$(has src-tauri/tauri.conf.json)"
} >> "$out"
