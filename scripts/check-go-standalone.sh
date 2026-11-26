#!/usr/bin/env bash
#
# check-go-standalone.sh — prove the standalone-Go-consumer recipe
# (REL8-1234, ADR-0592) end to end, the way `make test-go` proves the
# in-repo one.
#
# What it simulates, faithfully:
#   - the module as a consumer fetches it: `clients/go` is COPIED out of
#     the repository, so the cgo directives' `-L${SRCDIR}/../../target`
#     paths point at nothing — exactly the module-cache situation, where
#     the only `-L` that can resolve the library is the consumer's;
#   - the release archive's library, staged in one directory;
#   - the consumer contract at LINK time: one variable,
#     `CGO_LDFLAGS="-L<dir>"`. The header needs nothing (the module
#     carries its own copy — a drift test keeps it byte-equal), and the
#     library name comes from the module's own `-lrelate_ffi`. At RUN
#     time the same directory goes on the loader path, the separate
#     step clients/go/README.md documents.
# The program it runs is the documented create_match example, verbatim
# from docs/api/go/examples — so a link that silently resolved nothing
# cannot reach the example's output, and a documented example is proven
# in the standalone setting too.
#
# Usage:
#   scripts/check-go-standalone.sh
# Environment:
#   RELATE_LIB    the FFI library to stage (scripts/ffi-lib.sh applies
#                 its default: the repo's target/debug build — run
#                 `cargo build -p relate-ffi` first, which `make test-go`
#                 does)
#   GO            the go binary (default `go` on PATH)
#
# Exits 1 when the standalone project fails to build or run, 2 when the
# check cannot run (no go, the msvc link style, or no library — in that
# order: the platform answer comes first, then the tool, then the
# input).

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ffi-lib.sh owns the loader path and the library name for this OS:
# RELATE_LIB (what to stage), RELATE_LIB_DIR, RELATE_FFI_LINK (the
# msvc/cc discriminator) and RELATE_FFI_LOADER (the loader variable's
# name), so this script carries no uname case of its own.
# shellcheck source=ffi-lib.sh
source "$REPO/scripts/ffi-lib.sh"

# The mode gate first: a Windows contributor must hear that the check
# never covers that link style, not be sent to build a library first.
if [[ "$RELATE_FFI_LINK" == msvc ]]; then
  echo "check-go-standalone: the msvc link style needs the mingw" >&2
  echo "  import-library step in clients/go/README.md; this check covers" >&2
  echo "  the cc link style only" >&2
  exit 2
fi
command -v "${GO:-go}" >/dev/null 2>&1 || {
  echo "check-go-standalone: go not found on PATH" >&2
  exit 2
}
if [[ ! -f "$RELATE_LIB" ]]; then
  echo "check-go-standalone: no FFI library at $RELATE_LIB" >&2
  echo "  cargo build -p relate-ffi   (or set RELATE_LIB)" >&2
  exit 2
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/relate-go-standalone.XXXXXX")"
cleanup() { rm -rf "$SCRATCH"; }
trap cleanup EXIT

# The staged library — the release archive's one consumable half for a
# Go consumer (its header half is never on an include path: the module
# ships its own).
STAGE="$SCRATCH/relate-ffi"
mkdir -p "$STAGE"
cp "$RELATE_LIB" "$STAGE/"

# The module as a consumer sees it: a copy outside the repository, so no
# in-checkout target/ path exists for the cgo directives to find.
CONSUMER="$SCRATCH/consumer"
mkdir -p "$CONSUMER"
cp -R "$REPO/clients/go" "$CONSUMER/relate-client"

# The documented example, verbatim: it is `package main` importing only
# the module, and it prints the value it matched.
cp "$REPO/docs/api/go/examples/create_match.go" "$CONSUMER/main.go"

cat >"$CONSUMER/go.mod" <<'EOF'
module standalone.example

go 1.22

require github.com/relatedb/relate/clients/go v0.0.0

// The same pin clients/go/README.md documents for working against a
// checkout; a published version resolves the proxy tag instead
// (clients/go/vX.Y.Z, ADR-0592). The check's point is the link, not the
// version: the module sits OUTSIDE the repository, as the module cache
// puts it, so its own -L${SRCDIR}/../../target paths resolve nothing.
replace github.com/relatedb/relate/clients/go => ./relate-client
EOF

# The consumer contract at link time, exactly as documented:
# CGO_LDFLAGS carries only the -L directory (CGO_CFLAGS deliberately
# unset — the module's own header must compile it); the staged directory
# is the run-time load path, the separate step the README documents.
OUT="$(cd "$CONSUMER" &&
  env -u CGO_CFLAGS \
    CGO_LDFLAGS="-L$STAGE" \
    "$RELATE_FFI_LOADER=$STAGE" \
    "${GO:-go}" run .)"

if [[ "$OUT" != "Ada" ]]; then
  echo "check-go-standalone: unexpected example output: $OUT" >&2
  exit 1
fi

echo "check-go-standalone: clean (module outside the tree, staged library, CGO_LDFLAGS=-L only, example printed '$OUT')"
