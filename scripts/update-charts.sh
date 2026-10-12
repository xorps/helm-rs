#!/usr/bin/env bash
# Re-resolves the chart corpus (testdata/charts/sources.txt) and rewrites
# testdata/charts/charts.lock: latest version of every chart, sha256 of its
# .tgz, and whether the pinned `helm template` renders it (with
# testdata/charts/values/<name>.yaml when present).
#
# Usage:
#   scripts/update-charts.sh            # resolve latest versions of all charts
#   scripts/update-charts.sh --pinned   # keep locked versions; refresh render status
#   CHARTS=a,b scripts/update-charts.sh # only re-resolve these (others kept as locked)
set -euo pipefail
# shellcheck source=charts-common.sh
. "$(dirname "$0")/charts-common.sh"

pinned=0
[ "${1:-}" = "--pinned" ] && pinned=1

helm="$(helm_bin)"
helm_env
mkdir -p "$cache"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

wanted() { [ -z "${CHARTS:-}" ] || [[ ",$CHARTS," == *",$1,"* ]]; }

locked_line() { [ -f "$lock" ] && grep -F "\"name\": \"$1\"," "$lock" | head -1 || true; }

# Escapes a one-line message for embedding in JSON (and keeps `"` out so the
# line-based json_field parser stays valid).
json_msg() { tr '\n\t' '  ' | sed -e 's/\\/\\\\/g' -e "s/\"/'/g" | cut -c1-400; }

lines=()
while read -r name repo chart _; do
  case "$name" in ''|'#'*) continue ;; esac
  case "$repo" in oci://*) chart="${repo##*/}" ;; esac
  old="$(locked_line "$name")"
  version=""
  if [ $pinned = 1 ] || ! wanted "$name"; then
    version="$(json_field "$old" version)"
  fi
  file=""
  if [ -n "$version" ] && [ -f "$cache/$name-$version.tgz" ]; then
    file="$cache/$name-$version.tgz"
  else
    rm -rf "$tmp/pull"; mkdir -p "$tmp/pull"
    echo "resolving $name ($repo $chart${version:+ $version})" >&2
    if ! pull "$helm" "$tmp/pull" "$repo" "$chart" "$version" >"$tmp/pull.log" 2>&1; then
      echo "  FAILED to pull $name:" >&2; sed 's/^/    /' "$tmp/pull.log" >&2
      [ -n "$old" ] && lines+=("$old")
      continue
    fi
    got="$(ls "$tmp/pull"/*.tgz | head -1)"
    version="$(tar -xzOf "$got" "$chart/Chart.yaml" | sed -n 's/^version: *["'\'']\{0,1\}\([^"'\'' ]*\).*/\1/p' | head -1)"
    file="$cache/$name-$version.tgz"
    mv "$got" "$file"
  fi
  sum="$(sha256 "$file")"
  values="$charts_dir/values/$name.yaml"
  vargs=()
  vrel=""
  if [ -f "$values" ]; then vargs=(-f "$values"); vrel="values/$name.yaml"; fi
  if "$helm" template release-name "$file" ${vargs[@]+"${vargs[@]}"} >/dev/null 2>"$tmp/err"; then
    status="ok"; err=""
  else
    status="error"; err="$(json_msg <"$tmp/err")"
  fi
  echo "  $name $version $status${err:+: $err}" >&2
  lines+=("{\"name\": \"$name\", \"repo\": \"$repo\", \"chart\": \"$chart\", \"version\": \"$version\", \"file\": \"$name-$version.tgz\", \"sha256\": \"$sum\", \"values\": \"$vrel\", \"helm_template\": \"$status\", \"error\": \"$err\"}")
done <"$sources"

{
  echo "{"
  echo "\"helm_version\": \"$("$helm" version --template '{{.Version}}')\","
  echo "\"charts\": ["
  n=${#lines[@]}
  for i in "${!lines[@]}"; do
    sep=","; [ "$i" -eq $((n - 1)) ] && sep=""
    echo "${lines[$i]}$sep"
  done
  echo "]"
  echo "}"
} >"$lock.tmp"
mv "$lock.tmp" "$lock"
echo "wrote $lock (${#lines[@]} charts)" >&2
