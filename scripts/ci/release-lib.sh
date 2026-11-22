#!/usr/bin/env bash
# Shared helpers for the publish and prune scripts. Source this file; it defines functions only.

# version_gt <a> <b>: succeeds when version a is newer than b. Tags may start with "v". Compares
# MAJOR.MINOR.PATCH numerically; at equal core a release beats a pre-release, and two
# pre-releases compare by `sort -V` on their suffix.
version_gt() {
  local a="${1#v}" b="${2#v}" ac bc ap="" bp=""
  ac="${a%%-*}" bc="${b%%-*}"
  if [[ "$a" == *-* ]]; then ap="${a#*-}"; fi
  if [[ "$b" == *-* ]]; then bp="${b#*-}"; fi
  if [ "$ac" != "$bc" ]; then
    [ "$(printf '%s\n%s\n' "$ac" "$bc" | sort -V | tail -n1)" = "$ac" ]
    return
  fi
  if [ -z "$ap" ] && [ -n "$bp" ]; then return 0; fi
  if [ -n "$ap" ] && [ -z "$bp" ]; then return 1; fi
  [ "$ap" != "$bp" ] && [ "$(printf '%s\n%s\n' "$ap" "$bp" | sort -V | tail -n1)" = "$ap" ]
}

# channel_tags <experimental|main>: the v* release tags of one channel, one per line. The
# experimental channel is the -experimental tags; main is every other v* tag. Releases whose tag
# does not start with v (the updater feeds) are never listed.
channel_tags() {
  gh release list --limit 200 --json tagName --jq '.[].tagName' | while read -r tag; do
    [[ "$tag" == v* ]] || continue
    if [[ "$tag" == *-experimental* ]]; then
      [ "$1" = experimental ] && echo "$tag"
    else
      [ "$1" = main ] && echo "$tag"
    fi
  done
  return 0
}

# skip_if_superseded <channel> <tag>: exits 0 from the calling script, with a notice, when the
# channel already has this tag or a newer release, so a re-run of an older workflow can never
# publish an older build over a newer one. FORCE_FEED=true (exactly) disables the check.
skip_if_superseded() {
  local channel="$1" tag="$2" existing
  if [ "${FORCE_FEED:-}" = true ]; then
    echo "::notice::force_feed is true: publishing $tag without the newer-release check."
    return 0
  fi
  while read -r existing; do
    [ -n "$existing" ] || continue
    if [ "$existing" = "$tag" ] || version_gt "$existing" "$tag"; then
      echo "::notice::Not publishing $tag: the $channel channel already has $existing."
      exit 0
    fi
  done < <(channel_tags "$channel")
}
