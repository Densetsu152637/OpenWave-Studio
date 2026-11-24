#!/usr/bin/env bash
# Write one channel's updater feed: publish-feed.sh <experimental|main> <version> <release-tag>
# Runs after the versioned release exists, so the feed never names a missing asset, and before
# older releases are pruned. The calling workflow job holds `concurrency: updater-feed-<channel>`.
# The feed lives in the fixed release `updater-<channel>` (created if absent) as the asset
# latest.json, replaced with --clobber. The upload is skipped when the feed's version is greater
# than this one, so a late run of an older build can never roll the channel back; FORCE_FEED=true
# (exactly) disables that check for an intentional version reset. Every URL in the new feed must
# answer HTTP 200 (checked before the upload, so a broken feed is never published). Needs gh (GH_TOKEN, GH_REPO), jq and curl.
set -euo pipefail

channel="$1" version="$2" tag="$3"
here="$(dirname "$0")"
# shellcheck source=release-lib.sh
. "$here/release-lib.sh"
feed_tag="updater-$channel"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if ! gh release view "$feed_tag" > /dev/null 2>&1; then
  gh release create "$feed_tag" --title "Update feed ($channel)" --prerelease --latest=false \
    --target "${GITHUB_SHA:?}" \
    --notes "Holds only latest.json, the $channel updater feed. Written by CI; do not edit."
fi

current=""
if gh release download "$feed_tag" --pattern latest.json --output "$work/current.json" --clobber \
  > /dev/null 2>&1; then
  current="$(jq -r '.version // empty' "$work/current.json" 2> /dev/null || true)"
fi
if [ -n "$current" ] && [ "${FORCE_FEED:-}" != true ] && version_gt "$current" "$version"; then
  echo "::notice::Not updating the $channel feed: it is at $current, newer than $version."
  exit 0
fi
if [ "${FORCE_FEED:-}" = true ]; then
  echo "::notice::force_feed is true: writing the $channel feed ($current -> $version)."
fi

bash "$here/updater-manifest.sh" "$version" "$tag" "$work/latest.json"
bash "$here/check-feed.sh" "$work/latest.json"
gh release upload "$feed_tag" "$work/latest.json" --clobber
echo "Feed before: ${current:-none}; after: $version"

# The feed as users fetch it: its URL must serve the new version, and every URL in it must answer
# HTTP 200 (a freshly replaced asset can lag, so retry before failing).
public="https://github.com/${GITHUB_REPOSITORY:?}/releases/download/$feed_tag/latest.json"
for attempt in 1 2 3 4 5 6; do
  served="$(curl -fsSL "$public" | jq -r '.version // empty' || true)"
  [ "$served" = "$version" ] && break
  sleep $((attempt * 5))
done
if [ "$served" != "$version" ]; then
  echo "::error::$public serves version '${served:-none}', expected $version."
  exit 1
fi
bash "$here/check-feed.sh" "$public"
