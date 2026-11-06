#!/usr/bin/env bash
# Post the rendered loadtest comparison document as ONE self-updating PR
# comment (the loadtest-baselines CI presentation).
#
# Input: the document `compare-baselines.sh --render-only` (make
# render-loadtest-comparison) writes — same path resolution: `--doc PATH`
# wins, then COMPARISON_OUT, then $REPORTS_DIR/loadtest-comparison.md.
# The pull request is the positional argument or PR_NUMBER (the argument
# wins when both are given).
#
# The comment body starts with the marker
# `<!-- relate:loadtest-baselines -->` (an invisible HTML comment a
# re-run can find), then the title, an overall status line, the run URL
# and commit SHA, then the document. The status line is read from the
# document's producer-owned `**Status:** ok|warning` line — the
# authoritative overall verdict — never re-derived from the `**Pairs:**`
# tokens (those stay in the document for human readers): `ok` renders a
# note, `warning` renders a warning, and a missing or unrecognized status
# line is treated as warning — fail loud in presentation, never silently
# healthy.
#
# The marker match is author-filtered: only a comment authored by
# `github-actions[bot]` — the identity this job's GITHUB_TOKEN posts as —
# is ever PATCHed. A third-party comment that merely quotes the marker
# (a maintainer debugging, or a squatter trying to capture the
# comparison) is never edited by the bot; when no bot-authored marker
# comment exists the updater POSTs a fresh one, so a rerun never adds a
# duplicate and the bot never rewrites text it did not author. A missing
# document posts a short fallback body (marker included) saying the
# render step produced nothing.
#
# Presentation never gates the loadtest-baselines job: with no PR number
# (non-pull_request events) the script no-ops with exit 0, and any
# failure to post — gh missing, GITHUB_REPOSITORY unset, gh api error,
# network — prints to stderr and also exits 0. Only usage errors exit 2.
# (GitHub caps comment bodies at 65,536 characters; an oversized document
# fails through the same best-effort path.)
#
# Env:
#   PR_NUMBER             pull request number when no positional is given
#   GITHUB_REPOSITORY     owner/repo, the API base (required to post)
#   GITHUB_SERVER_URL     run URL component (GitHub Actions default)
#   GITHUB_RUN_ID         run URL component (GitHub Actions default)
#   GITHUB_SHA            commit SHA for the header (Actions default)
#   GH_TOKEN              gh auth (GitHub Actions: secrets.GITHUB_TOKEN)
#   COMPARISON_OUT        document path (default $REPORTS_DIR/loadtest-comparison.md)
#   REPORTS_DIR           document directory (default <repo>/target)
#
# usage: update-pr-comment.sh [<pr-number>] [--doc PATH] [--marker MARKER] [--title TITLE]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MARKER='<!-- relate:loadtest-baselines -->'
TITLE='Loadtest baselines comparison'
BOT_LOGIN='github-actions[bot]'

usage() {
  echo "usage: update-pr-comment.sh [<pr-number>] [--doc PATH] [--marker MARKER] [--title TITLE]" >&2
  exit 2
}

# --- arguments: optional PR number, optional document path -------------------

pr_arg=""
doc_arg=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --doc)
      [[ $# -ge 2 ]] || usage
      doc_arg="$2"
      shift 2
      ;;
    --doc=*)
      doc_arg="${1#--doc=}"
      shift
      ;;
    --marker)
      [[ $# -ge 2 ]] || usage
      MARKER="$2"
      shift 2
      ;;
    --marker=*)
      MARKER="${1#--marker=}"
      shift
      ;;
    --title)
      [[ $# -ge 2 ]] || usage
      TITLE="$2"
      shift 2
      ;;
    --title=*)
      TITLE="${1#--title=}"
      shift
      ;;
    -*) # unknown flags are usage errors, never silent no-ops
      usage
      ;;
    *)
      if [[ -n "$pr_arg" ]]; then
        usage # at most one positional
      fi
      pr_arg="$1"
      shift
      ;;
  esac
done

# Same document-path resolution as the render step; --doc (explicit) wins
# over COMPARISON_OUT (env) over the REPORTS_DIR default.
REPORTS_DIR="${REPORTS_DIR:-$ROOT/target}"
doc="${doc_arg:-${COMPARISON_OUT:-$REPORTS_DIR/loadtest-comparison.md}}"

pr="${pr_arg:-${PR_NUMBER:-}}"
if [[ -z "$pr" ]]; then
  # Non-pull_request events (schedule, workflow_dispatch, pushes to main).
  echo "update-pr-comment: no pull request number available (pass <pr-number> or set PR_NUMBER); not posting a comment"
  exit 0
fi
if [[ ! "$pr" =~ ^[1-9][0-9]*$ ]]; then
  echo "update-pr-comment: invalid pull request number: $pr" >&2
  usage
fi

repo="${GITHUB_REPOSITORY:-}"
if [[ -z "$repo" ]]; then
  echo "update-pr-comment: GITHUB_REPOSITORY is not set; cannot post a comment" >&2
  exit 0
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "update-pr-comment: gh CLI not found on PATH; cannot post a comment" >&2
  exit 0
fi

run_url=""
if [[ -n "${GITHUB_SERVER_URL:-}" && -n "${GITHUB_RUN_ID:-}" ]]; then
  run_url="$GITHUB_SERVER_URL/$repo/actions/runs/$GITHUB_RUN_ID"
fi
sha="${GITHUB_SHA:-}"

# Display path for messages: repo-relative under $ROOT, absolute elsewhere.
disp="$doc"
if [[ "$doc" == "$ROOT"/* ]]; then
  disp="${doc#"$ROOT"/}"
fi

# Overall status line from the document's producer-owned `**Status:**`
# line — the authoritative verdict the aggregator authors; the
# `**Pairs:**` tokens are never grepped. `ok` renders the note;
# `warning` — and, by the fail-loud rule, a missing or unrecognized
# status line — renders the warning.
status_line() {
  local line
  line="$(grep -m1 '^\*\*Status:\*\*' "$1" || true)"
  case "$line" in
    '**Status:** ok')
      if [[ "$TITLE" == "Memory controls and workspace memory calibration" ]]; then
        printf '%s\n' "> **Note:** All memory controls and workspace memory calibrations held."
      else
        printf '%s\n' "> **Note:** loadtest comparison completed with no failing rows."
      fi
      ;;
    '**Status:** no-baseline')
      printf '%s\n' "> **Warning:** no memory-controls envelope is frozen yet."
      ;;
    '**Status:** regression'|'**Status:** improvement'|'**Status:** mixed'|'**Status:** incompatible')
      printf '%s\n' "> **Warning:** memory controls reported ${line#**Status:** }."
      ;;
    *)
      printf '%s\n' "> **Warning:** loadtest comparison reported problems — see the flagged pairs below."
      ;;
  esac
}

# Status line when the document is missing: the render step produced
# nothing this run (fail loud in presentation, never silently healthy).
missing_doc_status() {
  # shellcheck disable=SC2016 # literal backticks: markdown code span
  printf '> **Warning:** the comparison document `%s` is missing — the render step produced nothing this run.\n' "$1"
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
body_file="$work/body.md"

# One document-presence decision drives the whole body: the status line
# and the content source both derive from it, so they can never disagree
# about whether the document exists. The content is a cat-able file either
# way — the document itself, or a scratch file carrying the unavailable
# note — which keeps the emission below straight-line.
status=""
content=""
if [[ -f "$doc" ]]; then
  status="$(status_line "$doc")"
  content="$doc"
else
  status="$(missing_doc_status "$disp")"
  content="$work/document-unavailable.md"
  printf '%s\n' "_(comparison document unavailable)_" >"$content"
fi

{
  printf '%s\n\n' "$MARKER"
  printf '%s\n\n' "**${TITLE}**"
  printf '%s\n' "$status"
  printf '\n'
  if [[ -n "$run_url" ]]; then
    printf -- '- **Run:** %s\n' "$run_url"
  fi
  if [[ -n "$sha" ]]; then
    printf -- '- **Commit:** %s\n' "$sha"
  fi
  printf '\n---\n\n'
  cat "$content"
} >"$body_file"

# The one gh-call protocol, shared by the list, PATCH, and POST branches:
# run `gh api` with the given arguments; on success echo the response
# body (the listed comment ids; the writes run --silent and echo nothing)
# and return 0; on failure print FAIL_MSG plus the captured gh stderr and
# return gh's exit status. Every caller keeps the best-effort contract —
# a posting failure exits 0: presentation never gates the job.
gh_call() {
  local fail_msg="$1"
  shift
  local out rc=0
  out="$(gh api "$@" 2>"$work/call.err")" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "update-pr-comment: $fail_msg:" >&2
    cat "$work/call.err" >&2
    return "$rc"
  fi
  if [[ -n "$out" ]]; then
    printf '%s\n' "$out"
  fi
  return 0
}

# Find this PR's marker comment: paginate the issue comments and keep the
# first id whose author is github-actions[bot] — the identity this job's
# GITHUB_TOKEN posts as — AND whose body carries the marker. The author
# filter is what keeps the bot from PATCHing a third-party comment that
# merely quotes the marker; with no bot-authored marker comment the flow
# below POSTs a fresh one instead (gh applies --jq per page and
# concatenates the results, so ids may span pages). jq string literals
# are double-quoted — single-quoted ones are a jq syntax error — and
# neither the login nor the marker contains a double quote.
list_filter=".[] | select(.user.login == \"$BOT_LOGIN\") | select((.body // \"\") | contains(\"$MARKER\")) | .id"
ids="$(gh_call "listing comments on PR #$pr failed" \
  --paginate --jq "$list_filter" "repos/$repo/issues/$pr/comments")" || exit 0
comment_id="${ids%%$'\n'*}"
if [[ -n "$comment_id" && ! "$comment_id" =~ ^[0-9]+$ ]]; then
  echo "update-pr-comment: unexpected gh output while searching for the marker comment: $comment_id" >&2
  exit 0
fi

if [[ -n "$comment_id" ]]; then
  # A bot-authored marker comment (the only kind the filter selects).
  if gh_call "updating comment $comment_id on PR #$pr failed" \
    --method PATCH "repos/$repo/issues/comments/$comment_id" \
    -F body=@"$body_file" --silent; then
    echo "update-pr-comment: updated comment $comment_id on PR #$pr"
  fi
  exit 0
fi

if gh_call "creating a comparison comment on PR #$pr failed" \
  --method POST "repos/$repo/issues/$pr/comments" \
  -F body=@"$body_file" --silent; then
  echo "update-pr-comment: created comparison comment on PR #$pr"
fi
exit 0
