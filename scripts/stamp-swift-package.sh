#!/usr/bin/env bash
# Stamp the Swift package's release binary into Package.swift (ADR-0433 §6).
#
# usage: scripts/stamp-swift-package.sh TAG ZIP
#
# Computes ZIP's SwiftPM checksum and rewrites the two release constants at
# the top of the repository-root Package.swift. The release workflow's
# `version` job runs this on the zip it then attaches to the GitHub release,
# so the tagged manifest pins exactly the bytes users download.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$REPO/Package.swift"

# The portable in-place sed, shared with scripts/release-bump.sh; one
# spelling, one owner.
# shellcheck source=lib/sedi.sh
source "$REPO/scripts/lib/sedi.sh"

if [[ $# -ne 2 ]]; then
  echo "usage: $0 TAG ZIP" >&2
  exit 2
fi
tag="$1"
zip="$2"
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$ ]] || {
  echo "not a release tag: $tag" >&2
  exit 2
}
[[ -f "$zip" ]] || { echo "no such zip: $zip" >&2; exit 2; }

checksum="$(swift package compute-checksum "$zip")"
[[ "$checksum" =~ ^[0-9a-f]{64}$ ]] || {
  echo "unexpected checksum output: $checksum" >&2
  exit 1
}

# Each constant must appear exactly once; anything else means the manifest
# shape changed and the stamp would silently miss.
for name in releaseTag releaseChecksum; do
  n="$(grep -c "^let ${name} = \"[^\"]*\"\$" "$MANIFEST" || true)"
  [[ "$n" == 1 ]] || {
    echo "Package.swift must declare exactly one 'let ${name} = \"…\"' line (found $n)" >&2
    exit 1
  }
done

sedi "$MANIFEST" \
  -e "s|^let releaseTag = \"[^\"]*\"\$|let releaseTag = \"${tag}\"|" \
  -e "s|^let releaseChecksum = \"[^\"]*\"\$|let releaseChecksum = \"${checksum}\"|"

grep -q "^let releaseTag = \"${tag}\"\$" "$MANIFEST"
grep -q "^let releaseChecksum = \"${checksum}\"\$" "$MANIFEST"
echo "$checksum"
