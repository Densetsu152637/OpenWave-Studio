#!/usr/bin/env bash
# Publish dist/ as an experimental pre-release: publish-experimental.sh <version> <sha>
# The updater feed is written here from stage 3 on; FORCE_FEED=true will then skip the
# "newer feed wins" check for one run.
set -euo pipefail

version="$1" sha="$2"
tag="v$version"
(cd dist && sha256sum -- * > SHA256SUMS)

gh release create "$tag" dist/* --target "$sha" --prerelease --latest=false \
  --title "$tag" \
  --notes "Experimental build of \`experimental\` at $sha. May be unstable. Installs on the experimental update channel update to the next experimental build automatically."

# One experimental release at a time: this one replaces the last.
bash scripts/ci/prune-releases.sh experimental "$tag"
