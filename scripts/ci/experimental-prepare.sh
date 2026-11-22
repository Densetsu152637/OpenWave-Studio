#!/usr/bin/env bash
# Compute the experimental release's version: X.Y.<run number>-experimental, where X.Y comes
# from the app's version (for example v0.1.9-experimental). The run number only grows, so each
# build updates the one before it. Needs a full-history checkout (fetch-depth: 0).
set -euo pipefail

out="${GITHUB_OUTPUT:-/dev/stdout}"
conf=src-tauri/tauri.conf.json

if [ "${GITHUB_REF:-}" != refs/heads/experimental ]; then
  echo "::error::Experimental releases are built from the experimental branch, not ${GITHUB_REF:-this ref}."
  exit 1
fi

# Only commits on experimental ship on the experimental channel.
if ! git merge-base --is-ancestor "${GITHUB_SHA:-HEAD}" origin/experimental; then
  echo "::error::${GITHUB_SHA:-HEAD} is not on origin/experimental."
  exit 1
fi

base="$(node -p "require('./$conf').version")"
base="${base%%-*}"
{
  echo "sha=$(git rev-parse HEAD)"
  echo "version=${base%.*}.${GITHUB_RUN_NUMBER:?}-experimental"
} >> "$out"
