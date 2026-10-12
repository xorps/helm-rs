#!/usr/bin/env bash
# Builds the Go oracle (real Helm, version pinned in oracle/go.mod) into target/helm-oracle.
set -euo pipefail
cd "$(dirname "$0")/../oracle"
GO="${GO:-go}"
if [ -z "${ASDF_GOLANG_VERSION:-}" ] && command -v asdf >/dev/null && [ -d "$HOME/.asdf/installs/golang/1.27.1" ]; then
  export ASDF_GOLANG_VERSION=1.27.1
fi
mkdir -p ../target
# Stamp the same version info as the official v4.3.0 release binary (see
# helm's Makefile), so `.Capabilities.HelmVersion` matches `helm template`.
V=helm.sh/helm/v4/internal/version
LDFLAGS="-X $V.version=v4.3.0 -X $V.gitCommit=bec5b06ed841fe5269972d864d5177944fd5970f -X $V.gitTreeState=clean"
"$GO" build -trimpath -ldflags "$LDFLAGS" -o ../target/helm-oracle .
echo "built $(cd .. && pwd)/target/helm-oracle"
