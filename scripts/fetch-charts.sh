#!/usr/bin/env bash
# Downloads the pinned chart corpus (testdata/charts/charts.lock) into
# testdata/charts/cache/, verifying each .tgz's sha256. Already-present files
# with the right checksum are skipped.
#
# Usage: scripts/fetch-charts.sh [name...]
set -euo pipefail
# shellcheck source=charts-common.sh
. "$(dirname "$0")/charts-common.sh"

helm="$(helm_bin)"
helm_env
mkdir -p "$cache"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

failed=0
while IFS= read -r line; do
  name="$(json_field "$line" name)"
  [ -n "$name" ] || continue
  if [ $# -gt 0 ] && ! printf '%s\n' "$@" | grep -qx "$name"; then continue; fi
  repo="$(json_field "$line" repo)"
  chart="$(json_field "$line" chart)"
  version="$(json_field "$line" version)"
  file="$cache/$(json_field "$line" file)"
  want="$(json_field "$line" sha256)"
  if [ -f "$file" ] && [ "$(sha256 "$file")" = "$want" ]; then
    continue
  fi
  echo "fetching $name $version" >&2
  rm -rf "$tmp/pull"; mkdir -p "$tmp/pull"
  if ! pull "$helm" "$tmp/pull" "$repo" "$chart" "$version" >"$tmp/log" 2>&1; then
    echo "  FAILED: $(cat "$tmp/log")" >&2; failed=1; continue
  fi
  got="$(ls "$tmp/pull"/*.tgz | head -1)"
  sum="$(sha256 "$got")"
  if [ "$sum" != "$want" ]; then
    echo "  CHECKSUM MISMATCH for $name $version: got $sum, want $want" >&2; failed=1; continue
  fi
  mv "$got" "$file"
done <"$lock"
[ $failed = 0 ] || { echo "some charts failed" >&2; exit 1; }
echo "charts in $cache are up to date" >&2
