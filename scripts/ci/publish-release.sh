#!/usr/bin/env bash
# Publish release-dist/ as a main-channel GitHub Release: publish-release.sh <tag> <prerelease:true|false>
# The updater feed is written here from stage 3 on; FORCE_FEED=true will then skip the
# "newer feed wins" check for one run.
set -euo pipefail

tag="$1" prerelease="$2"
(cd release-dist && sha256sum -- * > SHA256SUMS)

flags=(--title "$tag (main)" --generate-notes --verify-tag)
if [ "$prerelease" = true ]; then flags+=(--prerelease); else flags+=(--latest); fi
gh release create "$tag" "${flags[@]}" release-dist/*

# One main release at a time: a stable release replaces the last main release; a main
# pre-release replaces only the last pre-release.
if [ "$prerelease" = true ]; then
  bash scripts/ci/prune-releases.sh main-pre "$tag"
else
  bash scripts/ci/prune-releases.sh main "$tag"
fi
