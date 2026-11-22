#!/usr/bin/env bash
# Keep one release per channel: delete the channel's releases that are OLDER than the one just
# published, with their tags. Usage: prune-releases.sh <experimental|main|main-pre> <tag just published>
#   experimental  older vX.Y.N-experimental releases
#   main          older main releases (stable and pre-release)
#   main-pre      older main pre-releases only, keeping stable releases
# Newer releases and releases whose tag does not start with v (the updater feeds) are never touched.
set -euo pipefail

channel="$1" keep="$2"
# shellcheck source=release-lib.sh
. "$(dirname "$0")/release-lib.sh"

gh release list --limit 200 --json tagName,isPrerelease \
  --jq '.[] | "\(.tagName) \(.isPrerelease)"' |
  while read -r tag prerelease; do
    [ "$tag" = "$keep" ] && continue
    [[ "$tag" == v* ]] || continue
    experimental=false
    if [[ "$tag" == *-experimental* ]]; then experimental=true; fi
    case "$channel" in
      experimental) [ "$experimental" = true ] || continue ;;
      main) [ "$experimental" = false ] || continue ;;
      main-pre) [ "$experimental" = false ] && [ "$prerelease" = true ] || continue ;;
      *) echo "::error::unknown channel $channel" >&2; exit 1 ;;
    esac
    # Only replace older releases; a newer one stays.
    version_gt "$keep" "$tag" || continue
    echo "Replacing $tag with $keep"
    gh release delete "$tag" --yes --cleanup-tag
  done
