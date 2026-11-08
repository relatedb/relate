# Chain-lock tests for the release pipeline's deferred Slack announcement
# (ADR-0465).
#
# The wiring under test spans two workflow files: release.yml's release-notes
# call passes `publish: true, slack: false` — the generate run posts nothing
# inline and uploads slack-payload.json as the release-notes-slack-payload
# artifact — and release.yml's final `slack-announce` job downloads that
# artifact and posts it, but only after the whole tail (publish-releases-json,
# the self-update canary) is green. The website lives in the separate
# relate-web repository (ADR-0546): the tail's release-list leg is
# publish-releases-json, which attaches releases.json to the release
# assets and pokes relate-web to pull them — no gh-pages, no deploy job.
# release-notes.yml's inline Post-to-Slack step
# stays gated on `publish && slack`, so only a standalone publishing run
# (`slack` defaults to true) ever posts inline. Two shared invariants span
# the files the same way: the `release-notes-v<tag>` concurrency group —
# workflow-level on release-notes.yml, job-level on release.yml's
# slack-announce — serializes every run or job that can POST the
# non-idempotent payload (ADR-0465 §4), and the release-notes-slack-payload
# artifact name is the contract of the upload → download handoff.
# The handoff also moves the trust boundary: the payload leaves the
# generate runner as an artifact before any script-side scan runs, so
# release-notes.yml's Validate artifacts step scans it for the
# FIREWORKS_API_KEY value — the same fixed-string grep idiom as its
# notes.md scan — and fails the run before the upload step.
#
# These are chain locks, not semantic checks: each test anchors a
# fixed-string grep on the authored YAML (the job block's one-line `needs:`,
# the call's `slack:` input, the step's authored `if:`, each side's `group:`
# and artifact-`name:` line), so a regression — the announce job no longer
# waiting on the tail, the chained call posting inline, the inline step
# losing its slack gate, one side of the group string or the artifact name
# drifting — fails immediately, and a reformatted-but-equivalent spelling
# fails too, on purpose. Mutation-check the lock by editing the YAML and
# re-running.
#
# Local run:
#   bats .github/scripts/tests/release-pipeline-chain.bats
#
# Requirements:
#   bats-core >= 1.5.0  (brew install bats-core)
#   sed, grep (POSIX)

bats_require_minimum_version 1.5.0

TESTS_DIR="$(cd "$(dirname "$BATS_TEST_FILENAME")" && pwd)"
WORKFLOWS_DIR="$(cd "${TESTS_DIR}/../.." && pwd)/workflows"
RELEASE_YML="${WORKFLOWS_DIR}/release.yml"
RELEASE_NOTES_YML="${WORKFLOWS_DIR}/release-notes.yml"

# ---------------------------------------------------------------------------
# setup
# ---------------------------------------------------------------------------

setup() {
  [ -f "${RELEASE_YML}" ]
  [ -f "${RELEASE_NOTES_YML}" ]
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# The `needs:` line authored on release.yml's slack-announce job (flow-style
# list). Empty when the job or its needs list is gone. The sed range runs
# from the job id to the next two-space key (same-indent comments do not end
# it), so the grep can never pick up another job's needs line.
slack_announce_needs_line() {
  sed -n '/^  slack-announce:$/,/^  [^ #]/p' "${RELEASE_YML}" \
    | grep '^    needs:' || true
}

# The `needs:` line authored on release.yml's self-update-smoke job, same
# sed shape: the canary's own chain — it must wait for the release whose
# assets the inner upgrade downloads. Empty when the job or its needs list
# is gone.
self_update_smoke_needs_line() {
  sed -n '/^  self-update-smoke:$/,/^  [^ #]/p' "${RELEASE_YML}" \
    | grep '^    needs:' || true
}

# The `slack:` input line authored on release.yml's release-notes call (the
# one job that uses ./.github/workflows/release-notes.yml). Empty when the
# input is gone from the call.
chained_slack_line() {
  sed -n '/^  release-notes:$/,/^  [^ #]/p' "${RELEASE_YML}" \
    | grep '^      slack:' || true
}

# The `if:` gate line authored on release-notes.yml's Post-to-Slack step.
# Empty when the step or its gate is gone. The sed range runs from the step
# header to the next six-space line (the next step's header), so the grep
# sees only this step's keys.
post_to_slack_if_line() {
  sed -n '/^      - name: Post to Slack$/,/^      [^ ]/p' "${RELEASE_NOTES_YML}" \
    | grep '^        if:' || true
}

# The `group:` line authored on release.yml's slack-announce job-level
# concurrency block. Empty when the job, the block, or the group is gone.
# The sed range is the whole job (same range slack_announce_needs_line
# uses); the grep's 6-space anchor picks the concurrency block's keys —
# step content starts at 8.
slack_announce_group_line() {
  sed -n '/^  slack-announce:$/,/^  [^ #]/p' "${RELEASE_YML}" \
    | grep '^      group:' || true
}

# The `group:` line authored on release-notes.yml's workflow-level
# concurrency block. Empty when the block or the group is gone. The sed
# range runs from the top-level `concurrency:` key to the next top-level
# key (`jobs:`), so the grep sees only this block's keys.
release_notes_group_line() {
  sed -n '/^concurrency:$/,/^[^ #]/p' "${RELEASE_NOTES_YML}" \
    | grep '^  group:' || true
}

# The `name:` line authored on release-notes.yml's payload-upload step's
# `with:` block — the artifact name the announce job must download. Empty
# when the step or the name is gone. Same step-scoped sed range shape as
# post_to_slack_if_line.
payload_upload_name_line() {
  sed -n '/^      - name: Upload Slack payload for the announce job$/,/^      [^ ]/p' "${RELEASE_NOTES_YML}" \
    | grep '^          name:' || true
}

# The `name:` line authored on release.yml's payload-download step's
# `with:` block — the artifact name the release-notes run must upload.
# Empty when the step or the name is gone.
payload_download_name_line() {
  sed -n '/^      - name: Download the generated Slack payload$/,/^      [^ ]/p' "${RELEASE_YML}" \
    | grep '^          name:' || true
}

# The payload lines authored on release-notes.yml's Validate artifacts
# step — every line of that step's run block that mentions
# slack-payload.json (the producer-leg existence check and the secret
# scan). Empty when the step is gone. Same step-scoped sed range shape as
# post_to_slack_if_line.
validate_payload_lines() {
  sed -n '/^      - name: Validate artifacts$/,/^      [^ ]/p' "${RELEASE_NOTES_YML}" \
    | grep 'slack-payload.json' || true
}

# ---------------------------------------------------------------------------
# Tests: the announce job waits for the whole tail (ADR-0465)
# ---------------------------------------------------------------------------

@test "slack-announce needs publish-releases-json (the announcement waits for the release-list writer)" {
  # The tail's release-list leg is publish-releases-json (releases.json
  # rides the release assets, relate-web pulls them — ADR-0546): the
  # announcement must not post before the release list is written.
  needs_line="$(slack_announce_needs_line)"
  echo "needs line: ${needs_line}"
  [ -n "${needs_line}" ]
  grep -qF "publish-releases-json" <<< "${needs_line}"
}

@test "slack-announce does not need a deploy job (the site is relate-web's)" {
  # The lock on the ADR-0546 shape: no Pages deploy belongs to this
  # repository's release tail — relate-web deploys itself on the
  # repository_dispatch poke.
  needs_line="$(slack_announce_needs_line)"
  echo "needs line: ${needs_line}"
  [ -n "${needs_line}" ]
  ! grep -qF "deploy-pages" <<< "${needs_line}"
}

@test "self-update-smoke needs create-release (the canary downloads the new release's assets)" {
  needs_line="$(self_update_smoke_needs_line)"
  echo "needs line: ${needs_line}"
  [ -n "${needs_line}" ]
  grep -qF "create-release" <<< "${needs_line}"
}

@test "slack-announce needs self-update-smoke (the announcement waits for the canary)" {
  needs_line="$(slack_announce_needs_line)"
  echo "needs line: ${needs_line}"
  [ -n "${needs_line}" ]
  grep -qF "self-update-smoke" <<< "${needs_line}"
}

# ---------------------------------------------------------------------------
# Tests: the chained call defers, the inline step stays gated
# ---------------------------------------------------------------------------

@test "the chained release-notes call defers the Slack post (slack: false)" {
  slack_line="$(chained_slack_line)"
  echo "slack input line: ${slack_line}"
  [ -n "${slack_line}" ]
  grep -qF "slack: false" <<< "${slack_line}"
}

@test "release-notes.yml's Post-to-Slack step keeps its publish && slack gate" {
  if_line="$(post_to_slack_if_line)"
  echo "gate line: ${if_line}"
  [ -n "${if_line}" ]
  grep -qF "env.PUBLISH == 'true' && env.SLACK == 'true'" <<< "${if_line}"
}

# ---------------------------------------------------------------------------
# Tests: both posters serialize on the one release-notes-v<tag> group
# (ADR-0465 §4)
# ---------------------------------------------------------------------------

@test "release.yml's slack-announce job joins the release-notes-v<tag> group" {
  group_line="$(slack_announce_group_line)"
  echo "group line: ${group_line}"
  [ -n "${group_line}" ]
  grep -qF 'group: release-notes-v${{ needs.version.outputs.version }}' <<< "${group_line}"
}

@test "release-notes.yml's workflow-level group stays release-notes-v<tag>" {
  group_line="$(release_notes_group_line)"
  echo "group line: ${group_line}"
  [ -n "${group_line}" ]
  grep -qF "group: release-notes-\${{ format('v{0}', inputs.version) }}" <<< "${group_line}"
}

# ---------------------------------------------------------------------------
# Tests: the payload artifact handoff (upload name = download name)
# ---------------------------------------------------------------------------

@test "the payload artifact name is release-notes-slack-payload on both sides of the handoff" {
  upload_line="$(payload_upload_name_line)"
  download_line="$(payload_download_name_line)"
  echo "upload name line: ${upload_line}"
  echo "download name line: ${download_line}"
  [ -n "${upload_line}" ]
  [ -n "${download_line}" ]
  [ "${upload_line}" = "${download_line}" ]
  grep -qF 'name: release-notes-slack-payload' <<< "${upload_line}"
}

# ---------------------------------------------------------------------------
# Tests: the payload secret scan guards the artifact upload
# ---------------------------------------------------------------------------

@test "Validate artifacts scans the Slack payload for the FIREWORKS_API_KEY value" {
  payload_lines="$(validate_payload_lines)"
  echo "payload lines: ${payload_lines}"
  [ -n "${payload_lines}" ]
  # Fixed-string grep against the env var — the notes.md scan's idiom; the
  # secret value itself is never echoed.
  grep -qF 'grep -qF "${FIREWORKS_API_KEY}" target/release-notes/slack-payload.json' <<< "${payload_lines}"
}

@test "the payload secret scan is gated only on the key and the payload existing (every upload path)" {
  payload_lines="$(validate_payload_lines)"
  echo "payload lines: ${payload_lines}"
  [ -n "${payload_lines}" ]
  # No PUBLISH/SLACK term in the gate: the chained producer leg
  # (publish=true, slack=false), a slack=false manual run, and a dry run
  # all upload the artifact, so every one of them must scan it. The -s
  # term keeps non-producer paths tolerant of a missing payload.
  grep -qF 'if [ -n "${FIREWORKS_API_KEY}" ] && [ -s target/release-notes/slack-payload.json ] \' <<< "${payload_lines}"
}

@test "a secret-bearing payload fails closed with an ::error:: naming the payload file" {
  payload_lines="$(validate_payload_lines)"
  echo "payload lines: ${payload_lines}"
  [ -n "${payload_lines}" ]
  grep -qF '::error::target/release-notes/slack-payload.json contains the FIREWORKS_API_KEY value — refusing to upload' <<< "${payload_lines}"
}

@test "the payload secret scan sits before the upload step" {
  # The scan must fail the run before the artifact leaves the runner, so
  # lock the authored order: the scan's grep line precedes the upload
  # step's header line in release-notes.yml.
  scan_at="$(grep -nF 'grep -qF "${FIREWORKS_API_KEY}" target/release-notes/slack-payload.json' "${RELEASE_NOTES_YML}" | head -n 1 | cut -d: -f1)"
  upload_at="$(grep -n '^      - name: Upload Slack payload for the announce job$' "${RELEASE_NOTES_YML}" | cut -d: -f1)"
  echo "scan at line ${scan_at:-<gone>}, upload step at line ${upload_at:-<gone>}"
  [ -n "${scan_at}" ]
  [ -n "${upload_at}" ]
  [ "${scan_at}" -lt "${upload_at}" ]
}
