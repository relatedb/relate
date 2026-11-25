#!/usr/bin/env bash
# Gate a Linux x86-64 binary at the x86-64 baseline ISA.
#
# The released relate-linux-amd64 must run on every x86-64 CPU, so the ELF
# GNU property note ("x86 ISA needed") may list only x86-64-baseline. A
# higher level (x86-64-v2/v3/v4) means the build environment or a linked
# dependency put ISA-specific code into the binary. That refusal is
# unconditional: it fires with or without the vouch below.
#
# A binary with no "x86 ISA needed" note at all is a state the gate
# cannot classify on its own: the note is emitted solely by link inputs
# that declare an ISA need (CET-marked CRT objects, GCC compiled beyond
# -march=x86-64), and rustc emits it at no target-cpu — so a note-less
# binary is both the pinned rust:1.99-bookworm container's normal
# baseline link and what a raised target-cpu build would look like. The
# default is therefore fail-closed. The release lane accepts the state
# explicitly, with --pinned-toolchain, as its vouch that the build ran
# in the pinned container whose workflow also pins
# -C target-cpu=x86-64 — the compiler-level control the note channel
# cannot provide.
#
# Usage: check-linux-isa.sh [--pinned-toolchain] BINARY
set -euo pipefail

# The pin this vouch relies on lives in .cargo/config.toml — an env
# RUSTFLAGS would replace that table, so no build feeding this gate may
# set one (the mold preamble runs only on dev/test lanes).
allow_missing_note=0
# One optional flag, so no parsing loop: the first argument is the
# vouch, an unknown flag (refused), or absent (falls through to the
# usage check below); `${1-}` keeps set -u satisfied.
case "${1-}" in
  --pinned-toolchain)
    allow_missing_note=1
    shift
    ;;
  --*)
    echo "check-linux-isa: unknown flag: $1" >&2
    exit 2
    ;;
esac

if [[ $# -ne 1 ]]; then
  echo "usage: $0 [--pinned-toolchain] BINARY" >&2
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
  if [[ "$allow_missing_note" -eq 1 ]]; then
    echo "check-linux-isa: $binary: no 'x86 ISA needed' note (nothing beyond x86-64-baseline; accepted under --pinned-toolchain)"
    exit 0
  fi
  echo "check-linux-isa: $binary: no 'x86 ISA needed' GNU property note (the pinned rust:1.99-bookworm link emits none at baseline — pass --pinned-toolchain to accept that state)" >&2
  exit 1
fi

# The level list after the marker, classified whole: "x86-64-baseline"
# alone passes, any x86-64-vN token refuses, and every other shape — an
# unknown level beside baseline included — refuses as unclassifiable, so
# no verdict rides on the probes' order. The suffix split reuses the
# one marker literal the grep above matched.
levels=${line#*x86 ISA needed:}
levels=${levels// /}
case "$levels" in
  x86-64-baseline)
    echo "check-linux-isa: $binary: x86-64-baseline only"
    exit 0
    ;;
  *x86-64-v[0-9]*)
    higher=$(printf '%s\n' "$levels" | grep -oE 'x86-64-v[0-9]+' | paste -sd, -)
    echo "check-linux-isa: $binary: requires $higher; build at the x86-64 baseline ISA" >&2
    exit 1
    ;;
  *)
    echo "check-linux-isa: $binary: $line" >&2
    exit 1
    ;;
esac
