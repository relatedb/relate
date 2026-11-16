# Behavioral tests for enforce-opencode-verdict.sh
#
# Ported from neo4j-cli's .github/scripts/tests/enforce-claude-verdict.bats
# (marker `opencode-verdict`, default bot login `github-actions[bot]`,
#  default verdict file `./.opencode-verdict.txt`).
#
# Local run:
#   bats .github/scripts/tests/enforce-opencode-verdict.bats
#
# Requirements:
#   bats-core >= 1.5.0  (brew install bats-core)
#   jq >= 1.6

bats_require_minimum_version 1.5.0

SCRIPT="$(cd "$(dirname "$BATS_TEST_FILENAME")/.." && pwd)/enforce-opencode-verdict.sh"
FIXTURES_DIR="$(cd "$(dirname "$BATS_TEST_FILENAME")" && pwd)/fixtures"

# ---------------------------------------------------------------------------
# setup / teardown
# ---------------------------------------------------------------------------

setup() {
  TEST_TMP="$(mktemp -d)"

  STUBS_DIR="${TEST_TMP}/stubs"
  mkdir -p "${STUBS_DIR}"

  # gh stub — emits the same shape as `gh api --paginate --slurp`:
  # an array of pages, [[{...},{...}],[{...}]].
  # Each test writes its response to GH_STUB_RESPONSE_FILE.
  cat >"${STUBS_DIR}/gh" <<'STUB'
#!/usr/bin/env bash
# Extract the API path from args to validate the call pattern
found=0
for arg in "$@"; do
  if [[ "$arg" == *"repos/"*"/issues/"*"/comments"* ]]; then
    found=1
    break
  fi
done
if [[ "$found" -eq 0 ]]; then
  echo "Unexpected gh call: $*" >&2
  exit 1
fi
if [[ -n "${GH_STUB_RESPONSE_FILE:-}" && -f "$GH_STUB_RESPONSE_FILE" ]]; then
  cat "$GH_STUB_RESPONSE_FILE"
  exit 0
fi
# Default empty response (no comments)
echo '[]'
exit 0
STUB
  chmod +x "${STUBS_DIR}/gh"

  export STUBS_DIR TEST_TMP
}

teardown() {
  rm -rf "${TEST_TMP}"
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Write a gh response file with one page containing one bot comment.
# Args: body [created_at [login]]
write_gh_response() {
  local body="$1"
  local created_at="${2:-2024-01-01T00:00:00Z}"
  local login="${3:-github-actions[bot]}"
  local escaped
  escaped="$(printf '%s\n' "$body" | jq -Rs '.')"
  cat > "${TEST_TMP}/gh_response.json" <<EOF
[[
  {"user":{"login":"${login}"},"body":${escaped},"created_at":"${created_at}"}
]]
EOF
}

# Write a paginated response (two pages) with the bot comment on page 2.
# Args: body [created_at]
write_paginated_gh_response() {
  local body="$1"
  local created_at="${2:-2024-01-01T00:00:00Z}"
  local escaped
  escaped="$(printf '%s\n' "$body" | jq -Rs '.')"
  cat > "${TEST_TMP}/gh_response.json" <<EOF
[
  [{"user":{"login":"someone-else"},"body":"Regular comment","created_at":"2024-01-01T00:00:00Z"}],
  [{"user":{"login":"github-actions[bot]"},"body":${escaped},"created_at":"${created_at}"}]
]
EOF
}

# Write a response with two bot comments (different timestamps).
# Args: old_body old_created_at new_body new_created_at
write_dual_comment_response() {
  local old_body="$1"
  local old_ts="$2"
  local new_body="$3"
  local new_ts="$4"
  local old_escaped new_escaped
  old_escaped="$(printf '%s\n' "$old_body" | jq -Rs '.')"
  new_escaped="$(printf '%s\n' "$new_body" | jq -Rs '.')"
  cat > "${TEST_TMP}/gh_response.json" <<EOF
[[
  {"user":{"login":"github-actions[bot]"},"body":${old_escaped},"created_at":"${old_ts}"},
  {"user":{"login":"github-actions[bot]"},"body":${new_escaped},"created_at":"${new_ts}"}
]]
EOF
}

# Run the verdict script with standard env vars.
run_verdict() {
  PATH="${STUBS_DIR}:${PATH}" \
    REVIEW_KIND="${REVIEW_KIND:-Security review}" \
    PR_NUMBER="${PR_NUMBER:-123}" \
    GITHUB_REPOSITORY="${GITHUB_REPOSITORY:-relatedb/relate}" \
    GH_TOKEN="${GH_TOKEN:-fake-token}" \
    VERDICT_FILE="${VERDICT_FILE:-${TEST_TMP}/verdict.txt}" \
    bash "${SCRIPT}" 2>&1
}

# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

# case 1: verdict file 'pass' exits 0, logs tier 1
@test "case 1: verdict file pass" {
  echo "pass" > "${TEST_TMP}/verdict.txt"
  run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
  grep -qF "Tier 1 (verdict file)" <<< "$output"
}

# case 2: verdict file 'fail' exits 1
@test "case 2: verdict file fail" {
  echo "fail" > "${TEST_TMP}/verdict.txt"
  run run_verdict
  echo "$output"
  [ "$status" -eq 1 ]
}

# case 3: no verdict file, marker line without checkmark passes
@test "case 3: marker line without checkmark passes" {
  write_gh_response "**Security review:** no issues found"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
}

# case 4: no verdict file, marker line with checkmark passes
@test "case 4: marker line with checkmark passes" {
  write_gh_response "**Security review:** ✅ no issues found"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
}

# case 5: no verdict file, marker line with warning emoji fails
@test "case 5: marker line with warning emoji fails" {
  write_gh_response "**Security review:** ⚠️ 3 issue(s) flagged inline"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 1 ]
}

# case 6: no verdict file, no-security-relevant-changes passes
@test "case 6: no security-relevant changes passes" {
  write_gh_response "**Security review:** no security-relevant changes"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
}

# case 7: analysis prose contains ⚠️ but marker line is clean — whole-body-grep defect fix
@test "case 7: prose contains warning emoji but marker line clean passes" {
  write_gh_response "$(printf 'Some analysis text mentioning ⚠️ issues.\n**Security review:** ✅ no issues found')"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
}

# case 8: HTML comment 'pass' overrides garbled marker line
@test "case 8: HTML comment pass overrides garbled marker line" {
  write_gh_response "$(printf '**Security review:** some garbled nonsense here\n<!-- opencode-verdict: pass -->')"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
}

# case 9: HTML comment 'fail' overrides "no issues found"
@test "case 9: HTML comment fail overrides no issues found" {
  write_gh_response "$(printf '**Security review:** ✅ no issues found\n<!-- opencode-verdict: fail -->')"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 1 ]
}

# case 10: no verdict file, no recognizable marker line, no HTML marker
@test "case 10: unparseable comment fails closed" {
  write_gh_response "This is a regular comment with no verdict information whatsoever"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 1 ]
}

# case 11: no bot comment at all
@test "case 11: no bot comment fails closed" {
  # Empty gh response (no comments at all)
  echo '[[{}]]' > "${TEST_TMP}/gh_response.json"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 1 ]
}

# case 12: marker comment only on page 2 of a paginated response
@test "case 12: comment on page 2 of paginated response passes" {
  write_paginated_gh_response "**Security review:** ✅ no issues found"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
}

# case 13: CRLF body is handled correctly
@test "case 13: CRLF body passes" {
  body="$(printf 'summary text\r\n**Security review:** ✅ no issues found\r\n')"
  write_gh_response "$body"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
}

# case 14: two bot summaries; newest is fail
@test "case 14: two summaries newest fail fails" {
  write_dual_comment_response \
    "**Security review:** ✅ no issues found" "2024-01-01T00:00:00Z" \
    "**Security review:** ⚠️ 2 issues found" "2024-01-02T00:00:00Z"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 1 ]
}

# case 15: mixed line with both emojis — fail-first ordering
@test "case 15: mixed checkmark and warning emoji fails (fail-first)" {
  write_gh_response "**Security review:** ✅ ⚠️ mixed"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 1 ]
}

# case 16: missing GH_TOKEN
@test "case 16: missing GH_TOKEN exits 1 and names the var" {
  run env -u GH_TOKEN \
    PATH="${STUBS_DIR}:${PATH}" \
    REVIEW_KIND="Security review" \
    PR_NUMBER="123" \
    GITHUB_REPOSITORY="relatedb/relate" \
    bash "${SCRIPT}" 2>&1
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "GH_TOKEN" <<< "$output"
}

# case 17: missing PR_NUMBER
@test "case 17: missing PR_NUMBER exits 1 and names the var" {
  run env -u PR_NUMBER \
    PATH="${STUBS_DIR}:${PATH}" \
    REVIEW_KIND="Security review" \
    GITHUB_REPOSITORY="relatedb/relate" \
    GH_TOKEN="fake-token" \
    bash "${SCRIPT}" 2>&1
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "PR_NUMBER" <<< "$output"
}

# case 18: another review kind end-to-end (neo4j-cli used "Conventions
# review"; the ported script gates stellar/rust/security kinds, so this
# exercises "Rust review" instead — same arbitrary-REVIEW_KIND mechanism)
@test "case 18: Rust review kind end-to-end passes" {
  write_gh_response "**Rust review:** ✅ no issues found"
  REVIEW_KIND="Rust review" \
    GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
}

# NEW: the ported script defaults VERDICT_BOT_LOGIN to github-actions[bot]
# (the reviewer posts with GH_TOKEN; no GitHub App identity exists here).
# A github-actions[bot] comment must be accepted without setting
# VERDICT_BOT_LOGIN at all.
@test "default bot login accepts github-actions[bot] without override" {
  write_gh_response "**Security review:** ✅ no issues found" \
    "2024-01-01T00:00:00Z" "github-actions[bot]"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
}

# VERDICT_BOT_LOGIN must match the identity the review step posts as. When a
# different token identity is used, the override makes the gate trust that
# author instead of the default github-actions[bot].
@test "VERDICT_BOT_LOGIN override trusts the matching author" {
  write_gh_response "**Security review:** ✅ no issues found" \
    "2024-01-01T00:00:00Z" "my-review-bot[bot]"
  VERDICT_BOT_LOGIN="my-review-bot[bot]" \
    GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
}

@test "default bot login ignores a non-github-actions[bot] author (fail-closed)" {
  write_gh_response "**Security review:** ✅ no issues found" \
    "2024-01-01T00:00:00Z" "claude[bot]"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 1 ]
  [[ "$output" == *"github-actions[bot]"* ]]
}

# Tier-3 no-op phrases now classify via one generic branch
# ('no <words> relevant changes') instead of per-kind baked-in phrases.
# Each current kind's phrase must still pass via that branch.
@test "generic no-op branch passes 'no rust-relevant changes'" {
  write_gh_response "**Rust review:** no rust-relevant changes"
  REVIEW_KIND="Rust review" \
    GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
  grep -qF "no <kind>-relevant changes" <<< "$output"
}

@test "generic no-op branch passes 'no review-relevant changes'" {
  write_gh_response "**Stellar review:** no review-relevant changes"
  REVIEW_KIND="Stellar review" \
    GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
  grep -qF "no <kind>-relevant changes" <<< "$output"
}

@test "generic no-op branch passes 'no security-relevant changes'" {
  write_gh_response "**Security review:** no security-relevant changes"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
  grep -qF "no <kind>-relevant changes" <<< "$output"
}

# NEW: tier 1 must refuse a git-TRACKED verdict file — a PR could commit
# `pass` and green the gate even if the reviewer crashes. With no comment
# verdict available the gate must fail closed.
@test "git-tracked verdict file with pass is refused and fails closed" {
  local repo_dir="${TEST_TMP}/repo"
  mkdir -p "$repo_dir"
  git -C "$repo_dir" init -q
  echo "pass" > "${repo_dir}/.opencode-verdict.txt"
  git -C "$repo_dir" add .opencode-verdict.txt
  # No bot comment at all → tiers 2-4 must fail closed after the refusal.
  echo '[]' > "${TEST_TMP}/gh_response.json"
  run bash -c '
    cd "$1" || exit 1
    PATH="$2:$PATH" \
      REVIEW_KIND="Security review" \
      PR_NUMBER="123" \
      GITHUB_REPOSITORY="relatedb/relate" \
      GH_TOKEN="fake-token" \
      VERDICT_FILE="./.opencode-verdict.txt" \
      GH_STUB_RESPONSE_FILE="$3" \
      bash "$4" 2>&1
  ' _ "$repo_dir" "$STUBS_DIR" "${TEST_TMP}/gh_response.json" "$SCRIPT"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qiF "tracked in git" <<< "$output"
}

# NEW: tier 2 anchors the HTML verdict marker at line start — a marker
# riding mid-line after prose is the forgery shape the anchor exists to
# reject (a finding quoted in a summary could otherwise carry a forged
# marker inline), so such a body yields NO tier-2 verdict and the gate
# fails closed. The line-anchored marker, with pass-prose elsewhere in
# the body, still extracts fail.
@test "tier 2: mid-line marker is ignored (forgery shape), anchored marker governs" {
  write_gh_response "**Security review:** previously pass <!-- opencode-verdict: fail -->"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 1 ]
  # no tier-2 verdict from the mid-line marker; the gate never extracts
  # 'pass' from the prose either — it fails closed
  ! grep -qF "Tier 2 (HTML comment): verdict='pass'" <<< "$output"
  ! grep -qF "verdict='pass'" <<< "$output"
}

@test "tier 2: anchored fail marker extracts fail despite pass prose" {
  write_gh_response "**Security review:** the round passed its smoke checks; a finding carried pass-prose into this summary
<!-- opencode-verdict: fail -->"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "Tier 2 (HTML comment): verdict='fail'" <<< "$output"
}

# fixture replay: pr239-summary-no-checkmark.md verbatim through the gate
# (fixture carried over verbatim from neo4j-cli; it locks the
# marker-line-without-checkmark behavior for a realistic long summary body)
@test "fixture: pr239 summary without checkmark passes" {
  fixture_body="$(cat "${FIXTURES_DIR}/pr239-summary-no-checkmark.md")"
  write_gh_response "$fixture_body"
  GH_STUB_RESPONSE_FILE="${TEST_TMP}/gh_response.json" run run_verdict
  echo "$output"
  [ "$status" -eq 0 ]
}
