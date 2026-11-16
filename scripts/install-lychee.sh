#!/usr/bin/env bash
# Install the pinned lychee binary into /usr/local/bin. The link-check
# Sonic stage runs this as root on Linux; the case keys on
# platform-arch so the Linux assets are unreachable from other hosts.
# The tarball comes from the GitHub release API when a token is set
# (scripts/github-download.sh).
#
# The pinned digests: an unsigned release tarball is verified, never
# trusted. Bump procedure: set LYCHEE_VERSION below, then for each arch
#   sh scripts/github-download.sh --repo lycheeverse/lychee \
#     --tag "lychee-v${LYCHEE_VERSION}" \
#     --asset lychee-<arch>-unknown-linux-gnu.tar.gz \
#     -o lychee.tar.gz
#   sha256sum lychee.tar.gz
# and update the digest beside it below (lychee publishes no .sha256
# sidecar, so the digests are computed by hand at bump time).
set -euo pipefail

LYCHEE_VERSION=0.18.1
lychee_tag="lychee-v${LYCHEE_VERSION}"

here="$(cd "$(dirname "$0")" && pwd)"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    lychee_arch=x86_64
    SHA256=0d48569e2dc11c1917fa98321dfd7aa675fba03b79f41e8e74afa3223e30d379
    ;;
  Linux-aarch64|Linux-arm64)
    lychee_arch=aarch64
    SHA256=225908398609a7b552a20ecd67b6e88d1f4899ada51c3122d457d1740c70fe44
    ;;
  *) echo "install-lychee: unsupported host: $(uname -s)-$(uname -m) (Linux x86_64/aarch64 only)" >&2; exit 1 ;;
esac

asset="lychee-${lychee_arch}-unknown-linux-gnu.tar.gz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
sh "$here/github-download.sh" \
  --repo lycheeverse/lychee \
  --tag "$lychee_tag" \
  --asset "$asset" \
  -o "$work/lychee.tar.gz"
echo "$SHA256  $work/lychee.tar.gz" | sha256sum --check --quiet - >&2
tar --no-same-owner -xzf "$work/lychee.tar.gz" -C "$work"
bin="$(find "$work" -type f -name lychee | head -n 1)"
if [[ -z "$bin" ]]; then
  echo "install-lychee: archive did not contain lychee" >&2
  exit 1
fi
install -m 0755 "$bin" /usr/local/bin/lychee
