#!/usr/bin/env bash
# Install the pinned osv-scanner release that the nightly dependency
# advisory scan (.github/workflows/dependency-advisories.yml, ADR-0702)
# runs over every non-Rust lock file and manifest in the checkout.
#
# The binary comes from the google/osv-scanner release assets through
# scripts/github-download.sh, and each host's digest is pinned below: an
# unsigned release binary is verified, never trusted. The digest policy
# is the repo's one canonical helper (scripts/lib/common.sh, sha256_of).
#
# Bump procedure: set VERSION below, then for each host download its
# release binary, compute the binary's SHA-256 digest, and update the
# value beside it below. The release also publishes an
# osv-scanner_SHA256SUMS file; the pin is computed from the binary bytes,
# and a sums line that disagrees with a freshly computed pin is a
# re-download prompt, not an authority the install trusts.
#
# Usage: scripts/install-osv-scanner.sh DIR
#   Installs the osv-scanner binary into DIR and prints DIR. In GitHub
#   Actions, append the printed line to $GITHUB_PATH:
#     bash scripts/install-osv-scanner.sh "$RUNNER_TEMP/osv-scanner" >> "$GITHUB_PATH"
set -euo pipefail

VERSION=2.7.1

if [[ $# -ne 1 || "$1" == -h || "$1" == --help ]]; then
  echo "usage: $0 DIR" >&2
  exit 2
fi
DIR="$1"

# The release ships one bare binary per host; only the asset name, its
# digest and the installed name differ.
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64 | Linux-amd64)
    asset=osv-scanner_linux_amd64
    SHA256=fb7dab19a8bd9bbe5c3189f09d4b7ee1820c9553a23bac202648a56efd2f6ee8
    ;;
  Linux-aarch64 | Linux-arm64)
    asset=osv-scanner_linux_arm64
    SHA256=983d22fbad9724514e3fcc6acc83b5fa64c5ffa6b0d9f510028f22c1b223ff81
    ;;
  Darwin-x86_64)
    asset=osv-scanner_darwin_amd64
    SHA256=b8420d5485ba792626315f1bf9a5a2046e971d38eea75bf9090c38852dbc3b1b
    ;;
  Darwin-arm64 | Darwin-aarch64)
    asset=osv-scanner_darwin_arm64
    SHA256=b22c02877a811c8f78956e8643efa554932b7378f590d13f8e97d4e9448a29f3
    ;;
  MINGW*-x86_64 | MSYS*-x86_64 | CYGWIN*-x86_64)
    asset=osv-scanner_windows_amd64.exe
    SHA256=7121dcf420aefbbf89a7f0050c4d3094e079ff93afd334dfc96f16b02ec85800
    ;;
  *)
    echo "install-osv-scanner.sh: unsupported host $(uname -s)-$(uname -m)" >&2
    exit 1
    ;;
esac

here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib/common.sh
source "$here/lib/common.sh"

BIN=osv-scanner
[[ "$asset" == *.exe ]] && BIN=osv-scanner.exe
mkdir -p "$DIR"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

sh "$here/github-download.sh" \
  --repo google/osv-scanner \
  --tag "v$VERSION" \
  --asset "$asset" \
  -o "$work/$asset"
actual="$(sha256_of "$work/$asset")"
if [[ -z "$actual" || "$actual" != "$SHA256" ]]; then
  echo "install-osv-scanner.sh: sha256 mismatch for $asset (want $SHA256, got ${actual:-none})" >&2
  exit 1
fi

install -m 0755 "$work/$asset" "$DIR/$BIN"
"$DIR/$BIN" --version >&2
echo "$DIR"
