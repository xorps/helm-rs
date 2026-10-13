# Shared helpers for fetch-charts.sh / update-charts.sh (sourced, not run).
# shellcheck shell=bash

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
charts_dir="$root/testdata/charts"
cache="$charts_dir/cache"
lock="$charts_dir/charts.lock"
sources="$charts_dir/sources.txt"

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

helm_bin() {
  if [ -n "${HELM_BIN:-}" ]; then echo "$HELM_BIN"; return; fi
  "$root/scripts/fetch-helm.sh" 2>/dev/null | tail -1
}

# json_field <line> <key>: extracts a string field from a one-object-per-line
# JSON line as written by update-charts.sh (values never contain `"`).
json_field() {
  printf '%s\n' "$1" | sed -n "s/.*\"$2\": *\"\\([^\"]*\\)\".*/\\1/p"
}

# Isolated helm config/cache so the user's repos and registries don't matter.
helm_env() {
  export HELM_CONFIG_HOME="$root/.tools/helm-home/config"
  export HELM_CACHE_HOME="$root/.tools/helm-home/cache"
  export HELM_DATA_HOME="$root/.tools/helm-home/data"
  mkdir -p "$HELM_CONFIG_HOME" "$HELM_CACHE_HOME" "$HELM_DATA_HOME"
}

# pull <helm> <dest-dir> <repo-or-oci> <chart> [version]
pull() {
  local helm="$1" dest="$2" repo="$3" chart="$4" version="${5:-}"
  local args=()
  [ -n "$version" ] && args+=(--version "$version")
  case "$repo" in
    oci://*) "$helm" pull "$repo" ${args[@]+"${args[@]}"} -d "$dest" ;;
    *) "$helm" pull "$chart" --repo "$repo" ${args[@]+"${args[@]}"} -d "$dest" ;;
  esac
}
