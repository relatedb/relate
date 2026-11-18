#!/usr/bin/env bash
set -euo pipefail

# ============================================================
# release-notes-read.sh — the release-notes agent's ONE lens on git
# history and the notes files (the exfil fix: the agent's shell env
# holds the model API key, and a free-form `git show`/`wc` allowlist
# lets injected corpus text expand that key into a command argument,
# where git's error output — and through it the public job log —
# echoes it). The workflow's permission config allowlists exactly this
# wrapper; every argument is validated against BOTH a strict charset
# and a semantic SHAPE (a tag, a hex SHA, a repo path — a shape a
# secret never has) BEFORE any command runs, and a rejected argument is
# NEVER echoed back: an expanded secret dies in the rejection, unnamed,
# and a charset-valid secret never reaches git's error text either.
#
# Subcommands (the prompt's full vocabulary):
#   show REF:PATH    git show of a tagged path (a bare tag works too)
#   dig SHA          git show of a commit over the grounding doc paths
#   log RANGE        git log --oneline over PREV..RELEASE (tags/SHAs)
#   log30 TAG        the first-release fallback: 30 commits at a tag
#   lines PATH       wc -l of a file under target/release-notes/
#
# Usage: release-notes-read.sh SUBCOMMAND ARG
# ============================================================

usage() {
  echo "usage: release-notes-read.sh show REF[:PATH] | dig SHA | log RANGE | log30 TAG | lines target/release-notes/PATH" >&2
  exit 2
}

# Refuse an argument shape — the argument itself is never echoed: an
# injected value must not reach the job log, not even in a rejection.
reject() {
  echo "release-notes-read: refused: argument does not match the subcommand's shape" >&2
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

# A commit SHA: 7-64 hex characters.
is_sha() {
  case "$1" in
    *[!0-9a-fA-F]*) return 1 ;;
  esac
  [ "${#1}" -ge 7 ] && [ "${#1}" -le 64 ]
}

# A ref of either kind.
is_ref() {
  is_tag "$1" || is_sha "$1"
}

# A repository path: relative, no shell metacharacters.
is_path() {
  case "$1" in
    ""|/*|*[!A-Za-z0-9._/-]*) return 1 ;;
  esac
}

[ $# -eq 2 ] || usage
cmd="$1"
arg="$2"

case "$cmd" in
  show)
    # REF:PATH, or a bare REF. Splitting on the first colon keeps a
    # path's colons (there are none in this repository's paths).
    ref="${arg%%:*}"
    path="${arg#*:}"
    if [ "$ref" = "$arg" ]; then
      # No colon: the argument is the ref alone.
      is_ref "$arg" || reject
      exec git show "$arg"
    fi
    is_ref "$ref" || reject
    is_path "$path" || reject
    exec git show "$ref:$path"
    ;;
  dig)
    # A commit SHA; the doc path list is fixed here, never an argument.
    is_sha "$arg" || reject
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
    exec git log "${left}..${right}" --oneline
    ;;
  log30)
    is_tag "$arg" || reject
    exec git log -n 30 "$arg" --oneline
    ;;
  lines)
    # A notes file, and only a notes file.
    case "$arg" in
      target/release-notes/*) ;;
      *) reject ;;
    esac
    is_path "$arg" || reject
    exec wc -l "$arg"
    ;;
  *)
    usage
    ;;
esac
