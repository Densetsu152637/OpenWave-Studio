#!/usr/bin/env bash
# Publish release-dist/ as an experimental pre-release: publish-experimental.sh <version> <sha>
# Skips (exit 0) when the channel already has this version or a newer one. The updater feed is
# written here from stage 3 on. FORCE_FEED counts only when exactly "true"; it then also skips
# the newer-release check above.
set -euo pipefail

version="$1" sha="$2"
tag="v$version"
# shellcheck source=release-lib.sh
. "$(dirname "$0")/release-lib.sh"
skip_if_superseded experimental "$tag"

# SHA256SUMS covers the files published with the release; the update feed (latest.json, stage 3)
# lives in the updater-<channel> release and is not part of it.
(cd release-dist && sha256sum -- * > SHA256SUMS)

gh release create "$tag" release-dist/* --target "$sha" --prerelease --latest=false \
  --title "$tag" \
  --notes "Experimental build of \`experimental\` at $sha. May be unstable."

# One experimental release at a time: this one replaces the older ones.
bash scripts/ci/prune-releases.sh experimental "$tag"
