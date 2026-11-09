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

os="${RUNNER_OS:-}"
arch="${RUNNER_ARCH:-}"
if [[ -z "$os" ]]; then
  case "$(uname -s)" in
    Linux) os=Linux ;;
    Darwin) os=macOS ;;
    MINGW*|MSYS*|CYGWIN*) os=Windows ;;
    *) echo "install-sccache: unsupported OS $(uname -s)" >&2; exit 1 ;;
  esac
fi
if [[ -z "$arch" ]]; then
  case "$(uname -m)" in
    x86_64|amd64) arch=X64 ;;
    aarch64|arm64) arch=ARM64 ;;
    armv7l|armv7) arch=ARM ;;
    *) echo "install-sccache: unsupported arch $(uname -m)" >&2; exit 1 ;;
  esac
fi

case "${os}-${arch}" in
  Linux-X64) triple=x86_64-unknown-linux-musl; ext=tar.gz ;;
  Linux-ARM64) triple=aarch64-unknown-linux-musl; ext=tar.gz ;;
  Linux-ARM) triple=armv7-unknown-linux-musleabi; ext=tar.gz ;;
  macOS-X64) triple=x86_64-apple-darwin; ext=tar.gz ;;
  macOS-ARM64) triple=aarch64-apple-darwin; ext=tar.gz ;;
  Windows-X64) triple=x86_64-pc-windows-msvc; ext=zip ;;
  Windows-ARM64) triple=aarch64-pc-windows-msvc; ext=zip ;;
  *) echo "install-sccache: unsupported platform ${os}-${arch}" >&2; exit 1 ;;
esac

asset="sccache-${VERSION}-${triple}.${ext}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

sh "$here/github-download.sh" --repo mozilla/sccache --tag "$VERSION" --asset "$asset" -o "$work/$asset"
sh "$here/github-download.sh" --repo mozilla/sccache --tag "$VERSION" --asset "${asset}.sha256" -o "$work/sccache.sha256"

expected="$(awk '{print $1; exit}' "$work/sccache.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$work/$asset" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$work/$asset" | awk '{print $1}')"
else
  py=python3
  command -v python3 >/dev/null 2>&1 || py=python
  actual="$("$py" -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$work/$asset")"
fi
if [[ -z "$expected" || "$expected" != "$actual" ]]; then
  echo "install-sccache: checksum mismatch for ${asset}" >&2
  exit 1
fi

case "$ext" in
  zip)
    py=python3
    command -v python3 >/dev/null 2>&1 || py=python
    "$py" -c 'import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$work/$asset" "$work/unpack"
    ;;
  *)
    mkdir -p "$work/unpack"
    tar -xzf "$work/$asset" -C "$work/unpack"
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
