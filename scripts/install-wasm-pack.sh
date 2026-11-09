#!/usr/bin/env bash
# Install the pinned wasm-pack release into the cargo bin directory.
# Replaces taiki-e/install-action, whose tarball GET is anonymous.
# 0.13.1 publishes musl Linux (x86_64 and aarch64), Intel macOS, and
# x86_64 Windows. Other hosts get a clear error.
set -euo pipefail

VERSION=0.13.1
here="$(cd "$(dirname "$0")" && pwd)"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) triple=x86_64-unknown-linux-musl ;;
  Linux-aarch64|Linux-arm64) triple=aarch64-unknown-linux-musl ;;
  Darwin-x86_64) triple=x86_64-apple-darwin ;;
  MINGW*-x86_64|MSYS*-x86_64|CYGWIN*-x86_64) triple=x86_64-pc-windows-msvc ;;
  *) echo "install-wasm-pack: no prebuilt for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

asset="wasm-pack-v${VERSION}-${triple}.tar.gz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
sh "$here/github-download.sh" \
  --repo rustwasm/wasm-pack \
  --tag "v${VERSION}" \
  --asset "$asset" \
  -o "$work/$asset"
tar -xzf "$work/$asset" -C "$work"
bin="$(find "$work" -type f -name 'wasm-pack*' | head -n 1)"
if [[ -z "$bin" ]]; then
  echo "install-wasm-pack: archive did not contain wasm-pack" >&2
  exit 1
fi
bindir="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$bindir"
install -m 0755 "$bin" "$bindir/$(basename "$bin")"
echo "wasm-pack ${VERSION}"
