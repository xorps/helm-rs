#!/usr/bin/env bash
# Builds the Go oracle (real Helm, version pinned in oracle/go.mod) into target/helm-oracle.
set -euo pipefail
cd "$(dirname "$0")/../oracle"
GO="${GO:-go}"
if [ -z "${ASDF_GOLANG_VERSION:-}" ] && command -v asdf >/dev/null && [ -d "$HOME/.asdf/installs/golang/1.27.1" ]; then
  export ASDF_GOLANG_VERSION=1.27.1
fi
mkdir -p ../target
"$GO" build -trimpath -o ../target/helm-oracle .
echo "built $(cd .. && pwd)/target/helm-oracle"
