#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C.UTF-8

# ============================================================
# enforce-opencode-verdict.sh
#
# Shared verdict gate for the opencode review workflows
# (stellar, rust, security). Fail-closed: a missing or
# unparseable verdict fails the check so an early opencode
# exit or tool denial cannot silently pass.
#
# Ported from neo4j-cli's enforce-claude-verdict.sh.
#
# Fallback tiers (most exact to most tolerant):
#   1. VERDICT_FILE (default ./.opencode-verdict.txt) —
#      refused when the file is tracked in git, because a PR
#      could commit `pass` and green the gate even if the
#      reviewer crashes (tracked file → fall through to the
#      comment tiers); an untracked file (the reviewer writes
#      it at run time) works as before
#   2. HTML comment <!-- opencode-verdict: pass|fail -->
#      (token captured from inside the comment via a capture
#      group from the same expression that matched, so
#      pass/fail prose before the marker cannot mis-extract)
#   3. Marker-line phrase match (single-line only; the no-op
#      phrase is matched generically as
#      'no <words> relevant changes' — safe because this tier
#      is a fallback behind tiers 1-2 and the comment author
#      is pinned to VERDICT_BOT_LOGIN)
#   4. Fail-closed (::error:: + exit 1)
#
# The reviewer agent may post the mandated summary comment
# but skip the verdict-file write. The summary carries the
# review-kind marker line and may include the HTML comment;
# this script falls through until it reaches a verdict or
# exits 1.
#
# Env vars (required): REVIEW_KIND, PR_NUMBER,
#                      GITHUB_REPOSITORY, GH_TOKEN
# Env var (optional):  VERDICT_FILE
#                      (default ./.opencode-verdict.txt)
# Env var (optional):  VERDICT_BOT_LOGIN
#                      (default github-actions[bot]) — must
#                      match the identity the review step
#                      posts as
# ============================================================

# --- Input validation ---
for var in REVIEW_KIND PR_NUMBER GITHUB_REPOSITORY GH_TOKEN; do
  if [ -z "${!var:-}" ]; then
    echo "::error::Required env var ${var} is unset or empty"
    exit 1
  fi
done

for cmd in gh jq; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "::error::Required command '${cmd}' not found on PATH"
    exit 1
  fi
done

: "${VERDICT_FILE:=./.opencode-verdict.txt}"

# Author whose comment carries the verdict. Defaults to github-actions[bot]:
# the reviewer posts with GH_TOKEN and no GitHub App identity exists here.
# Only override when the review step authenticates with a different identity
# (see the `github_token` note in the review workflows) — a wider value lets a
# third party author a comment the gate will trust.
: "${VERDICT_BOT_LOGIN:=github-actions[bot]}"

verdict=""

# --- Tier 1: verdict file ---
if [ -s "$VERDICT_FILE" ]; then
  # A tracked verdict file is not trusted: a PR can commit `pass` and get a
  # green check even when the reviewer crashed (the Enforce step runs with
  # `if: always()`). Refuse it and fall through to the comment tiers. An
  # untracked file — the normal case, written by the reviewer at run time —
  # keeps working. git ls-files works on any checkout (including the
  # fetch-depth: 1 runner checkout) and fails cleanly outside a repo.
  # The invocation carries the full per-invocation git pin set (the
  # round-6 finding — the same six pins review-restore.sh, the staging
  # git show and the prep fetches carry): this step's shell runs after
  # the model step, and the restore choreography sourced before it has
  # already replaced .git/config with the minimal trusted configuration —
  # the pins hold the knobs closed regardless.
  if git -c core.fsmonitor= -c core.pager=cat -c core.worktree=. -c protocol.ext.allow=never -c core.attributesFile= -c core.hooksPath=/dev/null ls-files --error-unmatch "$VERDICT_FILE" >/dev/null 2>&1; then
    echo "Tier 1 (verdict file): '${VERDICT_FILE}' is tracked in git — refusing; falling through to comment tiers"
  else
    verdict="$(tr -d '[:space:]' < "$VERDICT_FILE")"
    echo "Tier 1 (verdict file): verdict='${verdict}'"
  fi
fi

# --- Tiers 2-4: comment fetch and analysis ---
if [ -z "$verdict" ]; then
  # Fetch raw API JSON separately so a gh failure produces a clear message
  # instead of a bare set -e abort (REQ-NF-005).
  api_json=""
  if ! api_json="$(gh api "repos/${GITHUB_REPOSITORY}/issues/${PR_NUMBER}/comments?per_page=100" \
                    --paginate --slurp)"; then
    echo "::error::Failed to fetch PR comments from GitHub API"
    exit 1
  fi

  # Extract the latest review-bot body whose own lane marker opens a
  # line — anchored at line start, never a substring match: a finding's
  # title quoting another lane's marker mid-line must not make the gate
  # read that lane's summary, and with several lanes' summaries on one
  # PR an unanchored contains() selects whichever posted newest, so a
  # failing lane could inherit a later lane's pass verdict.
  # --slurp + --paginate yields [[page1...],[page2...]] hence .[][] to flatten.
  # --arg passes the marker to jq; never shell-interpolate REVIEW_KIND into a
  # jq filter string (quoting/injection hazard).
  body=""
  if ! body="$(printf '%s\n' "$api_json" | jq -r --arg marker "**${REVIEW_KIND}:**" \
                                              --arg login "$VERDICT_BOT_LOGIN" '
    [ .[][] | select(.user.login == $login
                     and (.body | split("\n")
                          | any(.; sub("^[ \t]*"; "") | startswith($marker)))) ]
    | sort_by(.created_at) | last | .body // ""
  ')"; then
    echo "::error::Failed to parse PR comments with jq"
    exit 1
  fi

  if [ -z "$body" ]; then
    echo "::error::No ${VERDICT_BOT_LOGIN} comment with marker '**${REVIEW_KIND}:**' found"
    exit 1
  fi

  # CR-strip once; share across tier 2 and tier 3.
  # A comment body is attacker-influenceable, so use printf (not echo),
  # never eval, and never word-split (REQ-F-009).
  clean_body="$(printf '%s\n' "$body" | tr -d '\r')"

  # --- Tier 2: machine-readable HTML comment ---
  # grep -iE with hardcoded regex is safe here (not built from user data).
  # The match is anchored at line start (leading whitespace tolerated):
  # the verdict marker must OPEN a line, never ride mid-line — a finding
  # quoted in a summary could otherwise carry a forged marker inline.
  # grep returns 1 on no-match, which with set -euo pipefail aborts a $()
  # subshell on bash 3.2 (macOS runner). || true keeps the assignment safe.
  tier2_line="$(printf '%s\n' "$clean_body" \
    | grep -iE '^[[:space:]]*<!--[[:space:]]*opencode-verdict:[[:space:]]*(pass|fail)[[:space:]]*-->' \
    | tail -1 || true)"
  if [ -n "$tier2_line" ]; then
    # Capture the token from inside the HTML comment via a capture group from
    # the same expression that matched, so pass/fail prose before the marker
    # (e.g. 'previously pass <!-- opencode-verdict: fail -->') cannot
    # mis-extract. tr normalizes the captured token's case (BSD sed has no
    # \L). sed -n prints nothing on no-match; || true keeps the $() subshell
    # safe under set -e on bash 3.2.
    verdict="$(printf '%s\n' "$tier2_line" \
      | sed -nE 's/.*<!--[[:space:]]*opencode-verdict:[[:space:]]*(pass|fail)[[:space:]]*-->.*/\1/ip' \
      | tail -1 \
      | tr '[:upper:]' '[:lower:]' || true)"
    echo "Tier 2 (HTML comment): verdict='${verdict}'"
  fi

  # --- Tier 3: marker-line phrase match ---
  if [ -z "$verdict" ]; then
    # grep -F treats REVIEW_KIND as a literal (its ** are not regex meta-
    # characters under -F). Then check the line actually starts with the
    # marker via parameter-expansion prefix removal (no regex from the
    # body, no eval, no word-splitting).
    marker_prefix="**${REVIEW_KIND}:**"
    last_marker_line=""
    while IFS= read -r line; do
      # Remove leading whitespace
      trimmed="${line#"${line%%[![:space:]]*}"}"
      # Check prefix: if removing the prefix changes the value, it was present
      rest="${trimmed#"$marker_prefix"}"
      if [ "$rest" != "$trimmed" ]; then
        last_marker_line="$line"
      fi
    done < <(printf '%s\n' "$clean_body" | grep -F "$marker_prefix")

    if [ -n "$last_marker_line" ]; then
      # Strip leading whitespace for classification (single line only —
      # the whole-body grep is the defect this fixes).
      trimmed="${last_marker_line#"${last_marker_line%%[![:space:]]*}"}"
      # Fail indicators tested first
      if printf '%s\n' "$trimmed" | grep -qF '⚠'; then
        verdict="fail"
        echo "Tier 3 (marker line): verdict='fail' (contains ⚠)"
      elif printf '%s\n' "$trimmed" | grep -qiF 'flagged inline'; then
        verdict="fail"
        echo "Tier 3 (marker line): verdict='fail' (contains 'flagged inline')"
      elif printf '%s\n' "$trimmed" | grep -qiF 'no issues found'; then
        verdict="pass"
        echo "Tier 3 (marker line): verdict='pass' (contains 'no issues found')"
      elif printf '%s\n' "$trimmed" | grep -qiE 'no [a-z -]*relevant changes'; then
        # Generic no-op phrase ('no security-relevant changes',
        # 'no rust-relevant changes', 'no review-relevant changes', …) — one
        # tolerant branch instead of baking prompt vocabulary into the gate.
        # Safe: tier 3 is a fallback behind tiers 1-2 and the comment author
        # is pinned to VERDICT_BOT_LOGIN.
        verdict="pass"
        echo "Tier 3 (marker line): verdict='pass' (contains 'no <kind>-relevant changes')"
      elif printf '%s\n' "$trimmed" | grep -qF '✅'; then
        verdict="pass"
        echo "Tier 3 (marker line): verdict='pass' (contains ✅)"
      else
        echo "Tier 3 (marker line): unclassified — '${trimmed}'"
        unclassified_line="$trimmed"
      fi
    else
      echo "Tier 3 (marker line): no line starting with marker found"
    fi
  fi
fi

# --- Tier 4: fail-closed ---
if [ -z "$verdict" ]; then
  if [ -z "${body:-}" ]; then
    echo "::error::No ${VERDICT_BOT_LOGIN} comment with marker '**${REVIEW_KIND}:**' found"
  elif [ -n "${unclassified_line:-}" ]; then
    echo "::error::Marker line did not yield a known verdict: ${unclassified_line}"
  else
    echo "::error::No line starting with marker '**${REVIEW_KIND}:**' in ${VERDICT_BOT_LOGIN} comment"
  fi
  exit 1
fi

# --- Final verdict ---
if [ "$verdict" = "pass" ]; then
  exit 0
fi
echo "::error::opencode ${REVIEW_KIND} did not produce a pass verdict (verdict='${verdict}')"
exit 1
