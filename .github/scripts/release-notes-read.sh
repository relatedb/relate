#!/usr/bin/env bash
set -euo pipefail

# ============================================================
# release-notes-read.sh — the release-notes agent's ONE lens on git
# history and the notes files (the exfil fix: the agent's shell env
# holds the model API key, and a free-form `git show`/`wc` allowlist
# lets injected corpus text expand that key into a command argument,
# where git's error output — and through it the public job log —
# echoes it). The workflow's permission config allowlists exactly this
# wrapper. Three defenses stack, so no argument git or wc ever sees
# can carry a secret:
#
#   1. SHAPE: every argument matches a strict charset AND a semantic
#      grammar (a tag, a hex SHA, an allowlisted path — never a
#      free-form string; a hex-encoded secret is SHA-shaped, so shape
#      alone is not enough, which is why 2 and 3 exist).
#   2. SILENT EXISTENCE: every ref and path is resolved with stderr
#      discarded BEFORE the real command runs — a value that does not
#      name a real object is refused without echo, and a value that
#      does was never a secret.
#   3. ALLOWLIST: `show` reads only the six grounding files;
#      `lines` only the two artifacts, by exact name, after an
#      existence check — `wc`'s "No such file" text can never fire.
#
# A rejected argument is NEVER echoed: an injected value dies in the
# rejection, unnamed.
#
# Subcommands (the prompt's full vocabulary):
#   show REF:PATH    git show of a tagged allowlisted path (bare tag ok)
#   dig SHA          git show of a commit over the fixed doc paths
#   log RANGE        git log --oneline over PREV..RELEASE (tags/SHAs)
#   log30 TAG        the first-release fallback: 30 commits at a tag
#   lines PATH       wc -l of target/release-notes/notes.md or
#                    target/release-notes/slack-payload.json
#
# Usage: release-notes-read.sh SUBCOMMAND ARG
# ============================================================

usage() {
  echo "usage: release-notes-read.sh show REF[:PATH] | dig SHA | log RANGE | log30 TAG | lines target/release-notes/{notes.md,slack-payload.json}" >&2
  exit 2
}

# Refuse — the argument itself is never echoed: an injected value must
# not reach the job log, not even in a rejection.
reject() {
  echo "release-notes-read: refused: argument does not match the subcommand's shape or does not name a real object" >&2
  exit 2
}

# A release tag: v-prefixed dotted digits, optional pre-release tail.
is_tag() {
  case "$1" in
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *) return 1 ;;
  esac
  case "$1" in
    *[!A-Za-z0-9._-]*) return 1 ;;
  esac
}

# A commit SHA: 7-64 hex characters. (A hex-encoded secret shares this
# shape — which is why every ref is also existence-checked, silently,
# before git runs.)
is_sha() {
  case "$1" in
    *[!0-9a-fA-F]*) return 1 ;;
  esac
  [ "${#1}" -ge 7 ] && [ "${#1}" -le 64 ]
}

is_ref() {
  is_tag "$1" || is_sha "$1"
}

# The grounding paths `show` may read — the prompt's whole set, and
# nothing else: a secret-shaped "path" dies here.
is_grounding_path() {
  case "$1" in
    CHANGELOG.md|README.md|AGENTS.md|docs/api/cli/spec.md|docs/api/bolt/spec.md) ;;
    *) return 1 ;;
  esac
}

# Resolve a ref to a commit with stderr discarded: a value that does
# not name one is refused without echo.
resolves() {
  git rev-parse -q --verify "$1^{commit}" >/dev/null 2>&1
}

# Resolve a tagged path to an object with stderr discarded.
path_exists_at() {
  git cat-file -e "$1" 2>/dev/null
}

[ $# -eq 2 ] || usage
cmd="$1"
arg="$2"

case "$cmd" in
  show)
    # REF:PATH (PATH from the grounding allowlist), or a bare REF.
    # Splitting on the first colon keeps a path's colons (there are
    # none in the allowlisted names).
    ref="${arg%%:*}"
    path="${arg#*:}"
    if [ "$ref" = "$arg" ]; then
      # No colon: the argument is the ref alone.
      is_ref "$arg" || reject
      resolves "$arg" || reject
      exec git show "$arg"
    fi
    is_ref "$ref" || reject
    is_grounding_path "$path" || reject
    resolves "$ref" || reject
    path_exists_at "$ref:$path" || reject
    exec git show "$ref:$path"
    ;;
  dig)
    # A commit SHA; the doc path list is fixed here, never an argument.
    is_sha "$arg" || reject
    resolves "$arg" || reject
    exec git show "$arg" -- \
      README.md AGENTS.md CHANGELOG.md docs/api/cli/spec.md \
      docs/api/bolt/spec.md
    ;;
  log)
    # A range: two refs joined by ...
    left="${arg%%..*}"
    right="${arg#*..}"
    [ "$left" != "$arg" ] || reject
    [ -n "$left" ] || reject
    [ -n "$right" ] || reject
    is_ref "$left" || reject
    is_ref "$right" || reject
    resolves "$left" || reject
    resolves "$right" || reject
    exec git log "${left}..${right}" --oneline
    ;;
  log30)
    is_tag "$arg" || reject
    resolves "$arg" || reject
    exec git log -n 30 "$arg" --oneline
    ;;
  lines)
    # The two artifacts, by exact name — a traversal or a wildcard
    # shape dies here — and only if the file actually exists, so wc's
    # own "No such file" text can never fire.
    case "$arg" in
      target/release-notes/notes.md|target/release-notes/slack-payload.json) ;;
      *) reject ;;
    esac
    [ -f "$arg" ] || reject
    exec wc -l "$arg"
    ;;
  *)
    usage
    ;;
esac
