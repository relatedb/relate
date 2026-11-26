#!/usr/bin/env bash
# Install the pinned cargo-deny release into the cargo bin directory.
#
# The nightly advisory scan (.github/workflows/dependency-advisories.yml)
# runs `cargo deny check advisories` against both Cargo locks. The
# tarball comes from the EmbarkStudios release assets through
# scripts/github-download.sh (never an anonymous releases/download GET
# while a token is present), and each target's digest is pinned below:
# an unsigned release tarball is verified, never trusted.
#
# Bump procedure: set VERSION below, then for each target download its
# release tarball, compute the tarball's SHA-256 digest, and update the
# value beside it below. The release also publishes .sha256 sidecars;
# the pin is computed from the tarball bytes, and a sidecar that
# disagrees with a freshly computed pin is a re-download prompt, not an
# authority the install trusts.
#
# cargo-deny tags carry no `v` prefix: the 0.20.2 release is tag `0.20.2`.
set -euo pipefail

VERSION=0.20.2
here="$(cd "$(dirname "$0")" && pwd)"
# One canonical digest policy for every host-side installer: the
# sha256_of helper in scripts/lib/common.sh.
# shellcheck source=lib/common.sh
source "$here/lib/common.sh"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64)
    triple=x86_64-unknown-linux-musl
    SHA256=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f
    ;;
  Linux-aarch64|Linux-arm64)
    triple=aarch64-unknown-linux-musl
    SHA256=995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d
    ;;
  Darwin-x86_64)
    triple=x86_64-apple-darwin
    SHA256=248da7f581724e470071990c088ffc55c811981715f4cbdb258621fb79f8b7a6
    ;;
  Darwin-arm64|Darwin-aarch64)
    triple=aarch64-apple-darwin
    SHA256=fe67d82a10d8597a3549364cb733a3f9cc1bfff9031b7ae46384a9f2a72090c3
    ;;
  MINGW*-x86_64|MSYS*-x86_64|CYGWIN*-x86_64)
    triple=x86_64-pc-windows-msvc
    SHA256=975a22143262fd27476d19ee00c7af67978426e40e1dee94eed6bbade1cf87dc
    ;;
  *) echo "install-cargo-deny: unsupported platform $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

asset="cargo-deny-${VERSION}-${triple}.tar.gz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
sh "$here/github-download.sh" \
  --repo EmbarkStudios/cargo-deny \
  --tag "${VERSION}" \
  --asset "$asset" \
  -o "$work/$asset"

actual="$(sha256_of "$work/$asset")"
if [[ -z "$actual" || "$actual" != "$SHA256" ]]; then
  echo "install-cargo-deny: sha256 mismatch for $asset (want $SHA256, got ${actual:-none})" >&2
  exit 1
fi

tar --no-same-owner -xzf "$work/$asset" -C "$work"
# The exact binary names (the install-sccache.sh find shape): the
# downloaded tarball also matches a 'cargo-deny*' glob, and which one
# find lists first is filesystem order — never a race the install rides.
bin="$(find "$work" -type f \( -name cargo-deny -o -name cargo-deny.exe \) | head -n 1)"
if [[ -z "$bin" ]]; then
  echo "install-cargo-deny: archive did not contain cargo-deny" >&2
  exit 1
fi
bindir="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$bindir"
install -m 0755 "$bin" "$bindir/$(basename "$bin")"
echo "cargo-deny ${VERSION}"
