#!/usr/bin/env bash
# Downloads the official, pinned Helm release binary for this host into
# .tools/helm-v$HELM_VERSION/helm, verifying the sha256 published by the Helm
# project (pinned below, so a tampered mirror is detected).
#
# Usage: scripts/fetch-helm.sh            # prints the path of the binary
set -euo pipefail

HELM_VERSION=4.3.0
# sha256 of helm-v$HELM_VERSION-$os-$arch.tar.gz, from
# https://get.helm.sh/helm-v$HELM_VERSION-$os-$arch.tar.gz.sha256sum
checksum() {
  case "$1" in
    darwin-amd64) echo 347a784877e0e20eac865e8d1c36a80f6bb0861d6f29abd34defb6570ef95d92 ;;
    darwin-arm64) echo d3870437e1e95b67f8edbde964156c84a26503f560821d40c542441658934fba ;;
    linux-amd64)  echo 86584a54def73570558f66f5111cc53dfed56689637ae32c1201205d494f54fb ;;
    linux-arm64)  echo 31c5794dd55c66a51e6b7d2e2ac7a114ae8b1de41ff1d9ba51748ac973b06a08 ;;
    *) echo "unsupported platform $1" >&2; return 1 ;;
  esac
}

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/.tools/helm-v$HELM_VERSION"
bin="$dest/helm"

if [ -x "$bin" ] && "$bin" version --short 2>/dev/null | grep -q "^v$HELM_VERSION"; then
  echo "$bin"
  exit 0
fi

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$(uname -m)" in
  x86_64|amd64) arch=amd64 ;;
  arm64|aarch64) arch=arm64 ;;
  *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;;
esac
platform="$os-$arch"
want="$(checksum "$platform")"
tarball="helm-v$HELM_VERSION-$platform.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "downloading https://get.helm.sh/$tarball" >&2
curl -fsSL --retry 3 -o "$tmp/$tarball" "https://get.helm.sh/$tarball"
got="$(sha256 "$tmp/$tarball")"
if [ "$got" != "$want" ]; then
  echo "checksum mismatch for $tarball: got $got, want $want" >&2
  exit 1
fi
mkdir -p "$tmp/x"
tar -xzf "$tmp/$tarball" -C "$tmp/x"
mkdir -p "$dest"
install -m 0755 "$tmp/x/$platform/helm" "$bin"
"$bin" version --short >&2
echo "$bin"
