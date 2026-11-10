#!/usr/bin/env bash
# Install the pinned binaryen release (wasm-opt) that scripts/build-wasm.sh
# runs. CI uses this instead of apt: Ubuntu 24.04 ships binaryen 108, whose
# wasm-opt rewrites the export of a module's second table to point at the
# first. wasm-bindgen's externref table is such a second table (dev builds
# keep the target_features section that turns it on), so the bundle fails
# at init with "WebAssembly.Table.grow(): failed to grow table by 4".
# Fixed in binaryen 110.
#
# Usage: scripts/install-binaryen.sh DIR
#   Unpacks the release into DIR and prints DIR/bin. In GitHub Actions:
#     bash scripts/install-binaryen.sh "$RUNNER_TEMP/binaryen" >> "$GITHUB_PATH"
#
# Linux x86_64 and aarch64. On macOS: brew install binaryen.
set -euo pipefail

VERSION=133

if [[ $# -ne 1 || "$1" == -h || "$1" == --help ]]; then
  echo "usage: $0 DIR" >&2
  exit 2
fi
DIR="$1"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64 | Linux-amd64)
    arch=x86_64
    SHA256=2dc9c7813f5375db93d96ead4b78222fcc3e2677bbb832297af4797782a37489
    ;;
  Linux-aarch64 | Linux-arm64)
    arch=aarch64
    SHA256=89c07ea56faf38d0fbecf36ca8ec0721756716185f265b568e133d427f299bf8
    ;;
  *)
    echo "install-binaryen.sh supports Linux x86_64 and aarch64 only; on macOS: brew install binaryen" >&2
    exit 1
    ;;
esac

TARBALL="binaryen-version_${VERSION}-${arch}-linux.tar.gz"
here="$(cd "$(dirname "$0")" && pwd)"

mkdir -p "$DIR"
sh "$here/github-download.sh" \
  --repo WebAssembly/binaryen \
  --tag "version_${VERSION}" \
  --asset "$TARBALL" \
  -o "$DIR/$TARBALL"
echo "$SHA256  $DIR/$TARBALL" | sha256sum --check --quiet - >&2
tar --no-same-owner -xzf "$DIR/$TARBALL" -C "$DIR" --strip-components=1
rm "$DIR/$TARBALL"
"$DIR/bin/wasm-opt" --version >&2
echo "$DIR/bin"
