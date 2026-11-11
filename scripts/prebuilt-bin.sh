#!/usr/bin/env bash
# prebuilt-bin.sh — build the relate binary once per commit and stage it
# in the shared prebuilt directory (ADR-0251 §5), so the CLI e2e harness and
# the smoke lanes reuse ONE build instead of each recompiling their own.
#
# Usage:
#   scripts/prebuilt-bin.sh [--release] [--bolt]
#
#   --release   stage the release binary (default: dev/debug build)
#   --bolt      also stage the docker `entrypoint` (the set
#               `make docker DOCKER_PREBUILT_DIR=…` consumes); the flag
#               name predates LABS-381, ADR-0368, which retired the standalone
#               relate-bolt binary
#
# The binary lands in target/.prebuilt/<sha>[-dirty]/<profile>/ and the
# script prints the relate path on stdout — the last line is the path,
# so `export RELATE_BIN="$(scripts/prebuilt-bin.sh)"` wires every
# consumer (the CLI e2e harness, scripts/check-doc-examples.sh, the graph
# examples) to the shared build. A clean tree reuses the staged binary
# verbatim; a dirty tree rebuilds in place (cargo's incremental build makes
# the rebuild a no-op when nothing changed).
#
# Consumers and their resolution order:
#   CLI e2e harness (surfaces/relate-cli/tests/support/mod.rs):
#       RELATE_BIN, else the Cargo-built binary for the crate
#   scripts/check-doc-examples.sh:
#       RELATE_BIN, else target/.prebuilt/<sha>/<profile>/relate,
#       else target/release, else target/debug
#   make docker DOCKER_PREBUILT_DIR=target/.prebuilt/<sha>/release:
#       copies the staged musl-compatible binaries into the image instead of
#       the in-container chef build (see docs/testing/build-caches.md)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Loaded per the contract stated once in scripts/lib/usage.sh's header
# (existence check included).
usage_lib="$REPO/scripts/lib/usage.sh"
[ -f "$usage_lib" ] && [ -r "$usage_lib" ] || { echo "prebuilt-bin: cannot load scripts/lib/usage.sh (incomplete checkout?)" >&2; exit 2; }
# shellcheck source=lib/usage.sh
source "$usage_lib"

PROFILE="debug"
CARGO_PROFILE_ARGS=()
WITH_BOLT=0
for arg in "$@"; do
  case "$arg" in
    --release) PROFILE="release"; CARGO_PROFILE_ARGS=(--release) ;;
    --bolt) WITH_BOLT=1 ;;
    -h | --help)
      # The header comment block at the top of this script is the single
      # source of the usage prose; --help prints it verbatim via
      # scripts/lib/usage.sh's one printer.
      usage_text "${BASH_SOURCE[0]}"
      exit 0
      ;;
    *)
      echo "prebuilt-bin: unknown flag: $arg" >&2
      exit 2
      ;;
  esac
done

SHA="$(git -C "$REPO" rev-parse HEAD)"
DIRTY=""
if ! git -C "$REPO" diff --quiet HEAD -- 2>/dev/null; then
  DIRTY="-dirty"
fi
DEST="$REPO/target/.prebuilt/$SHA$DIRTY/$PROFILE"
BIN="$DEST/relate"

stage() {
  # $1 = binary name in target/<profile>/, $2 = package to build.
  # A clean tree at the same SHA reuses the staged binary; a dirty tree
  # always lets cargo decide (incremental no-op when nothing changed) and
  # re-stages when the build output is newer.
  local src="$REPO/target/$PROFILE/$1"
  if [[ -n "$DIRTY" || ! -x "$DEST/$1" ]]; then
    mkdir -p "$DEST"
    (cd "$REPO" && cargo build ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"} -p "$2" --bin "$1")
  fi
  if [[ "$src" -nt "$DEST/$1" ]]; then
    cp "$src" "$DEST/$1"
    chmod +x "$DEST/$1"
    echo "prebuilt-bin: staged $1 -> $DEST/$1" >&2
  fi
}

stage relate relate-cli
if ((WITH_BOLT)); then
  # `entrypoint` still lives in the relate-bolt crate (its lib); only
  # the crate's standalone server binary is retired (LABS-381, ADR-0368).
  stage entrypoint relate-bolt
fi

echo "$BIN"
