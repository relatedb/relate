#!/usr/bin/env bash
# Install the pinned lychee binary into /usr/local/bin. The link-check
# Sonic stage runs this as root. The tarball comes from the GitHub
# release API when a token is set (scripts/github-download.sh).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
case "$(uname -m)" in
  x86_64) lychee_arch=x86_64 ;;
  aarch64|arm64) lychee_arch=aarch64 ;;
  *) echo "install-lychee: unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

asset="lychee-${lychee_arch}-unknown-linux-gnu.tar.gz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
sh "$here/github-download.sh" \
  --repo lycheeverse/lychee \
  --tag lychee-v0.18.1 \
  --asset "$asset" \
  -o "$work/lychee.tar.gz"
tar -xzf "$work/lychee.tar.gz" -C "$work"
bin="$(find "$work" -type f -name lychee | head -n 1)"
if [[ -z "$bin" ]]; then
  echo "install-lychee: archive did not contain lychee" >&2
  exit 1
fi
install -m 0755 "$bin" /usr/local/bin/lychee
