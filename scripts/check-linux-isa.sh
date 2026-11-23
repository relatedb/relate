#!/usr/bin/env bash
# Gate a Linux x86-64 binary at the x86-64 baseline ISA.
#
# The released relate-linux-amd64 must run on every x86-64 CPU, so the ELF
# GNU property note ("x86 ISA needed") may list only x86-64-baseline. A
# higher level (x86-64-v2/v3/v4) means the build environment or a linked
# dependency put ISA-specific code into the binary.
#
# Usage: check-linux-isa.sh BINARY
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 BINARY" >&2
  exit 2
fi
binary=$1

if [[ ! -f "$binary" ]]; then
  echo "check-linux-isa: $binary: no such file" >&2
  exit 2
fi
if ! command -v readelf >/dev/null 2>&1; then
  echo "check-linux-isa: readelf not found (install binutils)" >&2
  exit 2
fi

if ! notes=$(LC_ALL=C readelf -n "$binary" 2>/dev/null); then
  echo "check-linux-isa: $binary: readelf failed (not an ELF file?)" >&2
  exit 1
fi

# One line: "Properties: x86 ISA needed: x86-64-baseline, x86-64-v2"
line=$(printf '%s\n' "$notes" | grep -m1 -F 'x86 ISA needed:' || true)
if [[ -z "$line" ]]; then
  echo "check-linux-isa: $binary: no 'x86 ISA needed' GNU property note" >&2
  exit 1
fi

higher=$(printf '%s\n' "$line" | grep -oE 'x86-64-v[0-9]+' || true)
if [[ -n "$higher" ]]; then
  echo "check-linux-isa: $binary: requires ${higher//$'\n'/,}; build at the x86-64 baseline ISA" >&2
  exit 1
fi
if [[ "$line" != *'x86-64-baseline'* ]]; then
  echo "check-linux-isa: $binary: $line" >&2
  exit 1
fi

echo "check-linux-isa: $binary: x86-64-baseline only"
