#!/usr/bin/env bash
# The one printer for a script's leading comment block — the whole header,
# which carries the usage prose and whatever notes the author kept beside it.
# The block is printed by content — skip the shebang, stop at the first
# non-comment line, strip one `# ` — so a line-numbered slice cannot drift
# when the header grows or shrinks. A caller whose --help should show the
# usage prose alone keeps its notes outside the header block.
# Source this file from a script that has resolved the repo root. The
# pre-check is part of the loading contract, and it is an existence and
# readability test rather than an `||` guard on the `source`: on bash 3.2
# (macOS) a `source` of a missing file under `set -e` aborts the script
# before any `||` guard can run — measured, exit 1 with the guard untouched —
# while bash 5 runs the guard. The pre-check behaves the same on both.
#   REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
#   usage_lib="$REPO/scripts/lib/usage.sh"
#   [ -f "$usage_lib" ] && [ -r "$usage_lib" ] || { echo "cannot load $usage_lib" >&2; exit 2; }
#   source "$usage_lib"
# Do not paste copies of this printer into callers.
#
# usage_text <script-path> — prints that script's header block to stdout.

usage_text() {
  awk 'NR == 1 && /^#!/ { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$1"
}
