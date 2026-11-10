#!/usr/bin/env bash
# Install the pinned cargo-nextest release into the cargo bin directory.
#
# The draft smoke lane used taiki-e/install-action, which curls the
# release tarball with no Authorization header. 0.9.151 is the version
# that action's v2 manifest called latest. The Sonic image pins 0.9.87
# on its own (see .sonic/docker/sonic-rust.Dockerfile).
set -euo pipefail

VERSION=0.9.151
here="$(cd "$(dirname "$0")" && pwd)"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) triple=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) triple=aarch64-unknown-linux-gnu ;;
  Darwin-arm64|Darwin-x86_64) triple=universal-apple-darwin ;;
  *) echo "install-nextest: unsupported platform $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

asset="cargo-nextest-${VERSION}-${triple}.tar.gz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
sh "$here/github-download.sh" \
  --repo nextest-rs/nextest \
  --tag "cargo-nextest-${VERSION}" \
  --asset "$asset" \
  -o "$work/$asset"
sh "$here/github-download.sh" \
  --repo nextest-rs/nextest \
  --tag "cargo-nextest-${VERSION}" \
  --asset "${asset%.tar.gz}.sha256" \
  -o "$work/nextest.sha256"

expected="$(awk '{print $1; exit}' "$work/nextest.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$work/$asset" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$work/$asset" | awk '{print $1}')"
fi
if [[ -z "$expected" || "$expected" != "$actual" ]]; then
  echo "install-nextest: checksum mismatch for ${asset}" >&2
  exit 1
fi

tar --no-same-owner -xzf "$work/$asset" -C "$work"
bin="$(find "$work" -type f -name cargo-nextest | head -n 1)"
if [[ -z "$bin" ]]; then
  echo "install-nextest: archive did not contain cargo-nextest" >&2
  exit 1
fi
bindir="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$bindir"
install -m 0755 "$bin" "$bindir/cargo-nextest"
echo "cargo-nextest ${VERSION}"
