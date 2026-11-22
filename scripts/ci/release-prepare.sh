#!/usr/bin/env bash
# Derive release outputs from a tag: release-prepare.sh <tag>
# Needs a full-history checkout (fetch-depth: 0) so the ancestry check can see origin/main.
set -euo pipefail

tag="$1"
out="${GITHUB_OUTPUT:-/dev/stdout}"
if [ ! -f src-tauri/tauri.conf.json ]; then
  echo "::error::No desktop app to release yet (src-tauri/tauri.conf.json is missing)."
  exit 1
fi

version="${tag#v}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "::error::Tag $tag is not a version tag (vMAJOR.MINOR.PATCH[-prerelease])."
  exit 1
fi

# The main channel only ships commits that are on main.
if ! git merge-base --is-ancestor "${GITHUB_SHA:-HEAD}" origin/main; then
  echo "::error::Tag $tag points at ${GITHUB_SHA:-HEAD}, which is not on main. Tag a commit on main."
  exit 1
fi

prerelease=false
if [[ "$version" == *-* ]]; then prerelease=true; fi
{
  echo "version=$version"
  echo "prerelease=$prerelease"
} >> "$out"
