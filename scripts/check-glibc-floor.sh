#!/usr/bin/env bash
# Gate the shipped Linux binary's glibc floor.
#
# The release promises glibc 2.35 or newer (docs/api/cli/spec.md,
# "Platform requirements"), so no versioned symbol the binary requires may
# ask for a newer glibc. This reads the highest GLIBC_x.y symbol version in
# the binary's dynamic symbol versions with `readelf --version-info` and
# prints it as GLIBC_x.y when it is within the documented floor, so the
# channel builders declare the floor the artifact actually has.
#
# Usage: check-glibc-floor.sh BINARY
set -euo pipefail

# The documented floor (docs/api/cli/spec.md, "Platform requirements").
floor_version=2.35
floor="GLIBC_$floor_version"

if [[ $# -ne 1 ]]; then
  echo "usage: $0 BINARY" >&2
  exit 2
fi
binary=$1

if [[ ! -f "$binary" ]]; then
  echo "check-glibc-floor: $binary: no such file" >&2
  exit 2
fi
if ! command -v readelf >/dev/null 2>&1; then
  echo "check-glibc-floor: readelf not found (install binutils)" >&2
  exit 2
fi

if ! info=$(LC_ALL=C readelf --version-info "$binary" 2>/dev/null); then
  echo "check-glibc-floor: $binary: readelf failed (not an ELF file?)" >&2
  exit 1
fi

# Every GLIBC_x.y[.z] token is a required symbol version; the version sort
# orders 2.9 before 2.34, which a lexical sort would not.
versions=$(printf '%s\n' "$info" \
  | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' | sort -uV || true)
if [[ -z "$versions" ]]; then
  echo "check-glibc-floor: $binary: no GLIBC_ symbol versions (not a glibc dynamic binary?)" >&2
  exit 1
fi

highest=$(printf '%s\n' "$versions" | tail -n 1)
highest_version=${highest#GLIBC_}
if [[ "$(printf '%s\n%s\n' "$floor_version" "$highest_version" | sort -V | tail -n 1)" != "$floor_version" ]]; then
  echo "check-glibc-floor: $binary: requires $highest, above the documented floor $floor" >&2
  exit 1
fi

echo "$highest"
