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
# cargo writes under CARGO_TARGET_DIR when the environment sets one (the
# ramdisk and shared-cache lanes do); a relative value resolves against the
# directory the build runs in, which is $REPO. Stated here beside the
# staging dir: the script's two path facts, in one place.
TARGET_ROOT="${CARGO_TARGET_DIR:-$REPO/target}"
[[ "$TARGET_ROOT" = /* ]] || TARGET_ROOT="$REPO/$TARGET_ROOT"

stage() {
  # $1 = binary name in target/<profile>/, $2 = package to build.
  # A clean tree at the same SHA reuses the staged binary; a dirty tree
  # always lets cargo decide (incremental no-op when nothing changed) and
  # re-stages when the build output is newer.
  local src="$TARGET_ROOT/$PROFILE/$1"
  # The build decision, recorded once: the publish gate below reads this
  # instead of re-spelling the same two-clause test.
  local built=0
  if [[ -n "$DIRTY" || ! -x "$DEST/$1" ]]; then
    mkdir -p "$DEST"
    (cd "$REPO" && cargo build ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"} -p "$2" --bin "$1")
    # Fail loud when a build that was supposed to produce the binary did not
    # (the reuse path below is untouched: no build, no expectation).
    [[ -f "$src" ]] || {
      echo "prebuilt-bin: cargo produced no $src" >&2
      exit 2
    }
    built=1
  fi
  # Publish when a build just ran, or when the source is newer than the
  # staged copy. `cp` does not preserve mtime, so a staged file that is not
  # executable (the residue an interrupted publish leaves) would otherwise
  # stay newer than $src and never be republished — the caller would get a
  # path its own -x probe rejects.
  if ((built)) || [[ "$src" -nt "$DEST/$1" ]]; then
    # Publish by rename, never in place: a lane exec'ing the staged path
    # meanwhile would otherwise hit ETXTBSY or read a truncated binary. The
    # temp follows the repo's writer contract (scripts/lib/toml.sh): mktemp
    # beside the target, removed when the publish does not complete.
    local tmp
    tmp=$(mktemp "$DEST/.$1.tmp.XXXXXX") || {
      echo "prebuilt-bin: cannot create a temp file beside $DEST/$1" >&2
      exit 2
    }
    cp "$src" "$tmp" && chmod +x "$tmp" && mv -f "$tmp" "$DEST/$1" || {
      rm -f "$tmp"
      echo "prebuilt-bin: could not publish $1" >&2
      exit 2
    }
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
