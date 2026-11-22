#!/usr/bin/env bash
# Publish release-dist/ as a main-channel GitHub Release: publish-release.sh <tag> <prerelease:true|false>
# Order: (1) the versioned release with all assets, (2) the main updater feed (stable releases
# only: a pre-release such as v0.2.0-rc.1 never feeds the stable channel), (3) prune older
# releases. Skips everything (exit 0) when the main channel already has this tag or a newer
# version. FORCE_FEED counts only when exactly "true"; it then also skips the newer-release check
# above and the feed's newer-version check (scripts/ci/publish-feed.sh).
set -euo pipefail

tag="$1" prerelease="$2"
# shellcheck source=release-lib.sh
. "$(dirname "$0")/release-lib.sh"
skip_if_superseded main "$tag"

# SHA256SUMS covers the files published with the release; the update feed (latest.json) lives
# in the updater-<channel> release and is not part of it.
(cd release-dist && sha256sum -- * > SHA256SUMS)

flags=(--title "$tag (main)" --generate-notes --verify-tag)
if [ "$prerelease" = true ]; then flags+=(--prerelease); else flags+=(--latest); fi
gh release create "$tag" "${flags[@]}" release-dist/*

if [ "$prerelease" != true ]; then
  bash "$(dirname "$0")/publish-feed.sh" main "${tag#v}" "$tag"
fi

# One main release at a time: a stable release replaces older main releases; a main pre-release
# replaces only older pre-releases.
if [ "$prerelease" = true ]; then
  bash scripts/ci/prune-releases.sh main-pre "$tag"
else
  bash scripts/ci/prune-releases.sh main "$tag"
fi
