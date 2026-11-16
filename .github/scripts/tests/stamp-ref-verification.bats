# Locks for the release pipeline's stamp-ref verification (the CodeQL
# cache-poisoning hardening of release.yml's ffi and npm lanes).
#
# The wiring under test: those lanes check out the swift-stamp job's
# output sha — a job output is untrusted input until proven otherwise —
# with the job token in scope and caches restored downstream, so each
# lane must prove the sha is the commit the stamp job pushed to main
# BEFORE its checkout step runs. The trust source is containment in
# branch-protected main (the release tag is cut only after every build
# is green, so there is no tag to consult at build time): the compare
# accepts exactly `ahead` (main contains the stamp commit) and
# `identical` (the stamp commit is main's tip) and rejects everything
# else loudly.
#
# These are chain locks, not semantic checks: each test anchors a
# fixed-string grep on the authored YAML (the step's header line, the
# checkout's ref line, the case arms and rejection text, the step env's
# token line), so a regression — a lane dropping the gate, moving it
# below the checkout, widening the acceptance, or swapping the
# read-only job token for a secret — fails immediately, and a
# reformatted-but-equivalent spelling fails too, on purpose.
# Mutation-check the lock by editing the YAML and re-running.
#
# Local run:
#   bats .github/scripts/tests/stamp-ref-verification.bats
#
# Requirements:
#   bats-core >= 1.5.0  (brew install bats-core)
#   sed, grep (POSIX)

bats_require_minimum_version 1.5.0

TESTS_DIR="$(cd "$(dirname "$BATS_TEST_FILENAME")" && pwd)"
WORKFLOWS_DIR="$(cd "${TESTS_DIR}/../.." && pwd)/workflows"
RELEASE_YML="${WORKFLOWS_DIR}/release.yml"

# The release lanes the hardening covers: ffi builds every FFI library
# leg and the three npm lanes pack the shipped packages.
VERIFIED_JOBS="ffi npm-platform npm-wasm npm-main"

# ---------------------------------------------------------------------------
# setup
# ---------------------------------------------------------------------------

setup() {
  [ -f "${RELEASE_YML}" ]
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# One job's authored block from release.yml (the job id to the next
# two-space key — same-indent comments do not end it), so the greps
# below can never pick up another job's lines.
job_block() {
  sed -n "/^  $1:\$/,/^  [^ #]/p" "${RELEASE_YML}"
}

# The verification step's header line number within a job's block (the
# `cut` field is the grep -n offset). Empty when the gate is gone.
verification_step_line() {
  job_block "$1" \
    | grep -n '^      - name: Verify the stamp sha is trusted main content$' | cut -d: -f1
}

# The job's stamp-sha checkout line number within its block. Empty when
# the lane no longer builds from the stamped release commit.
stamp_checkout_line() {
  job_block "$1" \
    | grep -nF 'ref: ${{ needs.swift-stamp.outputs.sha }}' | cut -d: -f1
}

# The verification step's own block (the step header to the next
# step's header line), for the greps that lock the step's content.
verification_step_block() {
  job_block "$1" \
    | sed -n '/^      - name: Verify the stamp sha is trusted main content$/,/^      [^ ]/p'
}

# ---------------------------------------------------------------------------
# Tests: every covered lane gates the checkout
# ---------------------------------------------------------------------------

@test "ffi verifies the stamp sha before checking it out" {
  step="$(verification_step_line ffi)"
  checkout="$(stamp_checkout_line ffi)"
  echo "verify step at block line ${step:-<gone>}, stamp checkout at ${checkout:-<gone>}"
  [ -n "${step}" ]
  [ -n "${checkout}" ]
  [ "${step}" -lt "${checkout}" ]
}

@test "npm-platform verifies the stamp sha before checking it out" {
  step="$(verification_step_line npm-platform)"
  checkout="$(stamp_checkout_line npm-platform)"
  echo "verify step at block line ${step:-<gone>}, stamp checkout at ${checkout:-<gone>}"
  [ -n "${step}" ]
  [ -n "${checkout}" ]
  [ "${step}" -lt "${checkout}" ]
}

@test "npm-wasm verifies the stamp sha before checking it out" {
  step="$(verification_step_line npm-wasm)"
  checkout="$(stamp_checkout_line npm-wasm)"
  echo "verify step at block line ${step:-<gone>}, stamp checkout at ${checkout:-<gone>}"
  [ -n "${step}" ]
  [ -n "${checkout}" ]
  [ "${step}" -lt "${checkout}" ]
}

@test "npm-main verifies the stamp sha before checking it out" {
  step="$(verification_step_line npm-main)"
  checkout="$(stamp_checkout_line npm-main)"
  echo "verify step at block line ${step:-<gone>}, stamp checkout at ${checkout:-<gone>}"
  [ -n "${step}" ]
  [ -n "${checkout}" ]
  [ "${step}" -lt "${checkout}" ]
}

# ---------------------------------------------------------------------------
# Tests: what the gate accepts and what it rejects
# ---------------------------------------------------------------------------

@test "the gate accepts exactly main-contained stamp shas (ahead | identical)" {
  # Containment in branch-protected main is the trust source — the
  # pre-tag equivalent of consulting the release tag — so the case
  # arms are exactly the two contained statuses.
  grep -qF 'ahead | identical)' "${RELEASE_YML}"
}

@test "a stamp sha that is not on main fails closed with its own diagnosis" {
  grep -qF 'is not on ${GITHUB_REPOSITORY}@main (compare: ${status}) — refusing the unverified checkout' "${RELEASE_YML}"
}

@test "a stamp sha that does not resolve fails closed with its own diagnosis" {
  # An invented or unpushed sha never reaches the compare: the gh api
  # failure path names it separately from the not-on-main rejection.
  grep -qF 'does not resolve against ${GITHUB_REPOSITORY} main — not the commit swift-stamp pushed' "${RELEASE_YML}"
}

# ---------------------------------------------------------------------------
# Tests: the gate is least-privilege
# ---------------------------------------------------------------------------

@test "every gate runs on the read-only job token and carries no secrets" {
  for job in ${VERIFIED_JOBS}; do
    block="$(verification_step_block "${job}")"
    echo "== ${job}"
    echo "${block}"
    [ -n "${block}" ]
    grep -qF 'GH_TOKEN: ${{ github.token }}' <<< "${block}"
    ! grep -q 'secrets\.' <<< "${block}"
  done
}
