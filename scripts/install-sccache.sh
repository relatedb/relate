#!/usr/bin/env bash
# Install the pinned sccache release onto a private bin directory and
# print that directory. The CI preamble adds it to GITHUB_PATH.
#
# The old mozilla-actions/sccache-action step resolved the tag with the
# job token, then fetched the tarball from releases/download with no
# Authorization header. That anonymous GET is what 504s. This script
# downloads the tarball and its .sha256 sidecar through scripts/github-download.sh.
#
# Pinned at v0.19.0 (the release the action resolved when the anonymous
# download failed). Asset names match that action's platform map.
set -euo pipefail

VERSION=v0.19.0
here="$(cd "$(dirname "$0")" && pwd)"

# The host to a release triple and archive extension, the sibling
# installers' one shape (uname's own vocabulary): `uname -s -m` straight
# to the asset name the release publishes.
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64 | Linux-amd64) triple=x86_64-unknown-linux-musl; ext=tar.gz ;;
  Linux-aarch64 | Linux-arm64) triple=aarch64-unknown-linux-musl; ext=tar.gz ;;
  Linux-armv7l | Linux-armv7) triple=armv7-unknown-linux-musleabi; ext=tar.gz ;;
  Darwin-x86_64) triple=x86_64-apple-darwin; ext=tar.gz ;;
  Darwin-arm64) triple=aarch64-apple-darwin; ext=tar.gz ;;
  MINGW*-x86_64 | MSYS*-x86_64 | CYGWIN*-x86_64) triple=x86_64-pc-windows-msvc; ext=zip ;;
  MINGW*-aarch64 | MSYS*-aarch64 | CYGWIN*-aarch64) triple=aarch64-pc-windows-msvc; ext=zip ;;
  *) echo "install-sccache: unsupported platform $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

asset="sccache-${VERSION}-${triple}.${ext}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The digest policy is the repo's one canonical helper (scripts/lib/
# common.sh, download_verified): it downloads the tarball and its .sha256
# sidecar and refuses a mismatch, so this installer keeps only its unpack
# arm.
# shellcheck source=lib/common.sh
. "$here/lib/common.sh"
download_verified "$here/github-download.sh" \
  mozilla/sccache "$VERSION" "$asset" "$work/$asset" "${asset}.sha256"

case "$ext" in
  zip)
    "$(python_interpreter)" -c 'import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$work/$asset" "$work/unpack"
    ;;
  *)
    mkdir -p "$work/unpack"
    tar --no-same-owner -xzf "$work/$asset" -C "$work/unpack"
    ;;
esac

bin="$(find "$work/unpack" -type f \( -name sccache -o -name sccache.exe \) | head -n 1)"
if [[ -z "$bin" ]]; then
  echo "install-sccache: archive did not contain sccache" >&2
  exit 1
fi

out="${RUNNER_TEMP:-/tmp}/sccache-bin"
mkdir -p "$out"
cp "$bin" "$out/"
chmod 0755 "$out/$(basename "$bin")"
echo "$out"
