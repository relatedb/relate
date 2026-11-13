#!/usr/bin/env bash
# Shared portable in-place sed — the one spelling of the GNU/BSD dance the
# release scripts both need (release-bump.sh's version sweep, the Swift
# package stamp in scripts/stamp-swift-package.sh). Source this file from a
# script that has resolved the repo root, e.g.:
#   REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
#   source "$REPO/scripts/lib/sedi.sh"
# Do not paste copies of this helper into callers.
#
# -i.bak is accepted by both GNU sed and BSD sed, so the same call works on
# the GNU-sed CI runners and on a macOS rehearsal. The sidecar is the
# helper's own business: sedi removes it before returning, so callers never
# see it. sedi_remove_sidecars is that one cleanup shape; its only caller
# besides sedi is the interrupted-run path (release-bump.sh's rollback),
# which cleans through it rather than spelling the suffix — the same
# pairing that rollback already has with toml_remove_sidecars.
#
# Usage: sedi <file> <sed-args...>

# Remove the sidecar a sedi call leaves beside <file>.
# Usage: sedi_remove_sidecars <file>
sedi_remove_sidecars() {
  rm -f "$1.bak"
}

# In-place sed. The exit status is sed's own, never the cleanup's: a caller
# that does not run `set -e` still sees a failed rewrite as a failure, and a
# caller that does aborts at the call site exactly as before. (`rc`, not
# `status` — that name is a read-only special parameter in zsh, and this
# file is sourced, so a caller's shell owns the namespace.)
# Usage: sedi <file> <sed-args...>
sedi() {
  local file=$1 rc=0
  shift
  sed -i.bak "$@" "$file" || rc=$?
  sedi_remove_sidecars "$file"
  return "$rc"
}
