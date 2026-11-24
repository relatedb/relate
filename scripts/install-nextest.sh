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


# shellcheck source=lib/common.sh
. "$here/lib/common.sh"
# The host key is common.sh's; this installer keeps only its asset map.
case "$(host_key || true)" in
  linux-x86_64) triple=x86_64-unknown-linux-gnu ;;
  linux-aarch64) triple=aarch64-unknown-linux-gnu ;;
  darwin-aarch64 | darwin-x86_64) triple=universal-apple-darwin ;;
  *) host_refuse install-nextest || exit 1 ;;
esac

asset="cargo-nextest-${VERSION}-${triple}.tar.gz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# The digest policy is the repo's one canonical helper (scripts/lib/
# common.sh, download_verified): it downloads the tarball and its .sha256
# sidecar and refuses a mismatch, so this installer keeps only its unpack
# arm.
download_verified "$here/github-download.sh" \
  nextest-rs/nextest "cargo-nextest-${VERSION}" \
  "$asset" "$work/$asset" "${asset%.tar.gz}.sha256"

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
