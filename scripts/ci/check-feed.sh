#!/usr/bin/env bash
# Check an updater feed: check-feed.sh <latest.json path or URL>
# Fails unless every URL in the feed's platforms answers HTTP 200 (redirects followed). Each URL
# is retried for a while, because a freshly uploaded release asset can take a moment to be served.
set -euo pipefail

source="$1"
if [[ "$source" == http* ]]; then
  feed="$(curl -fsSL --retry 5 --retry-delay 5 "$source")"
else
  feed="$(cat "$source")"
fi

mapfile -t urls < <(jq -r '[.platforms[].url] | unique[]' <<< "$feed")
if [ "${#urls[@]}" -eq 0 ]; then
  echo "::error::The feed lists no platform URLs."
  exit 1
fi

failed=0
for url in "${urls[@]}"; do
  code=000
  for attempt in 1 2 3 4 5 6; do
    code="$(curl -sIL -o /dev/null -w '%{http_code}' --max-time 60 "$url" || true)"
    [ "$code" = 200 ] && break
    sleep $((attempt * 5))
  done
  if [ "$code" = 200 ]; then
    echo "200 $url"
  else
    echo "::error::HTTP $code for $url"
    failed=1
  fi
done
exit "$failed"
