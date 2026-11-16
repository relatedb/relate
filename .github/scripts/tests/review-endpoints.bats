# Behavioral tests for lib/review-endpoints.sh — the one CI endpoint
# fetch and merge-base resolution the review pre- and post-steps run
# (stellar-prep.sh and opencode-review-prep.sh; ADR-0271, ADR-0603).
# The fetches carry the full per-invocation git pin set (the round-6
# finding, the same six flags review-restore.sh and the workflow's
# staging git show carry), and the round-6 transport payload below
# pins the load-bearing one shut: a model-written url.<url>.insteadOf
# can rewrite the origin URL to an ext:: command — refused by
# -c protocol.ext.allow=never even when the model also planted
# protocol.ext.allow=always (probed on git 2.43 and 2.54: with both
# planted and no pin, the ext:: command executes). The round-9 tests
# pin the two findings that share this library: the credential rides
# git's env-numbered configuration channel through git_authed — never
# the command line a surviving model-spawned process could read
# through ps — and the hooksPath pin keeps a model-crafted .git/hooks
# script from executing on the fetch path.
#
# Local run:
#   bats .github/scripts/tests/review-endpoints.bats
#
# Requirements:
#   bats-core >= 1.5.0  (brew install bats-core)
#   git

bats_require_minimum_version 1.5.0

LIB="$(cd "$(dirname "$BATS_TEST_FILENAME")/.." && pwd)/lib/review-endpoints.sh"

setup() {
  TEST_TMP="$(mktemp -d)"
  # Hermetic HOME/XDG: no test may consult the developer's real
  # gitconfig, and the payload test must be the only config source.
  export HOME="${TEST_TMP}/home"
  export XDG_CONFIG_HOME="${TEST_TMP}/xdg"
  mkdir -p "${HOME}" "${XDG_CONFIG_HOME}"
}

teardown() {
  rm -rf "${TEST_TMP}"
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# The fetch fixture: a scratch work repo whose two commits stand in for
# the PR endpoints, plus a bare clone as its origin remote. The bare
# mirror allows fetching the base SHA exactly the way GitHub does
# (uploadpack.allowReachableSHA1InWant — an ancestor SHA fetch, which a
# default local bare refuses). Exports BASE_SHA (the first commit) and
# HEAD_SHA (the second).
make_fetch_repo() {
  REPO_DIR="${TEST_TMP}/repo"
  mkdir -p "${REPO_DIR}"
  git -C "${REPO_DIR}" init -q
  git -C "${REPO_DIR}" config user.email review-endpoints@test
  git -C "${REPO_DIR}" config user.name "review-endpoints test"
  echo base > "${REPO_DIR}/tracked.txt"
  git -C "${REPO_DIR}" add tracked.txt
  git -C "${REPO_DIR}" commit -qm base
  BASE_SHA="$(git -C "${REPO_DIR}" rev-parse HEAD)"
  echo head > "${REPO_DIR}/tracked.txt"
  git -C "${REPO_DIR}" add tracked.txt
  git -C "${REPO_DIR}" commit -qm head
  HEAD_SHA="$(git -C "${REPO_DIR}" rev-parse HEAD)"
  git clone -q --bare "${REPO_DIR}" "${TEST_TMP}/origin.git"
  # GitHub's server allows fetching any reachable SHA; a default local
  # bare does not — mirror the server the production fetch talks to
  git -C "${TEST_TMP}/origin.git" config uploadpack.allowReachableSHA1InWant true
  git -C "${REPO_DIR}" remote add origin "${TEST_TMP}/origin.git"
  git -C "${REPO_DIR}" fetch -q origin
  export BASE_SHA HEAD_SHA
}

# A model-written "hook" script and its observable marker: the transport
# payload below points an insteadOf rewrite at it and asserts the marker
# never appears.
plant_marker_hook() {
  EVIL="${TEST_TMP}/model-payload.sh"
  MARKER="${TEST_TMP}/payload-ran"
  printf '#!/bin/sh\ntouch "%s"\n' "${MARKER}" > "${EVIL}"
  chmod +x "${EVIL}"
}

# The round-9 payload: real hook scripts a model shell plants in the
# default .git/hooks directory — a pre-*, a post-* and the
# reference-transaction an undefended `git reset --hard` executes
# (probed on git 2.43/2.54; no hook fires on any command the fetch
# path itself runs, so this is the posture assertion for the path —
# if a future git consults a hook during fetch, the test catches it).
plant_model_hooks() {
  printf '#!/bin/sh\ntouch "%s"\n' "${TEST_TMP}/pre-commit-ran" > "${REPO_DIR}/.git/hooks/pre-commit"
  printf '#!/bin/sh\ntouch "%s"\n' "${TEST_TMP}/post-checkout-ran" > "${REPO_DIR}/.git/hooks/post-checkout"
  printf '#!/bin/sh\ntouch "%s"\n' "${TEST_TMP}/reference-transaction-ran" > "${REPO_DIR}/.git/hooks/reference-transaction"
  chmod +x "${REPO_DIR}/.git/hooks/pre-commit" "${REPO_DIR}/.git/hooks/post-checkout" "${REPO_DIR}/.git/hooks/reference-transaction"
}

# The post steps' fetch path: source the lib and call fetch_review_base
# the way stellar-prep.sh and opencode-review-prep.sh do — under their
# set -euo pipefail, so a refused transport is a loud step failure.
run_fetch() {
  run bash -c '
    set -euo pipefail
    cd "$1" || exit 1
    . "$2"
    fetch_review_base "AUTHORIZATION: basic stub" "$3" "$4"
  ' _ "${REPO_DIR}" "${LIB}" "${BASE_SHA}" "${HEAD_SHA}"
}

# ---------------------------------------------------------------------------
# The round-6 transport payload
# ---------------------------------------------------------------------------

@test "a url.insteadOf rewrite to ext:: with a planted protocol.ext.allow=always never executes — the pinned fetch refuses the transport" {
  make_fetch_repo
  plant_marker_hook
  # the round-6 payload: the model shell wrote .git/config during the
  # model step — an insteadOf entry rewriting the origin URL to an
  # ext:: command (which runs it), and protocol.ext.allow=always to
  # enable the transport git otherwise asks about. Without the
  # per-invocation pin this executes the model's command under the
  # post step's write token (probed on git 2.43 and 2.54: the marker
  # appears); with -c protocol.ext.allow=never the fetch is refused
  # loudly instead — the step fails closed, never silently.
  git -C "${REPO_DIR}" config "url.ext::sh ${EVIL}".insteadOf "${TEST_TMP}/origin.git"
  git -C "${REPO_DIR}" config protocol.ext.allow always
  run_fetch
  [ "$status" -ne 0 ]
  [[ "$output" == *"transport 'ext' not allowed"* ]]
  # the model's command never executed
  [ ! -e "${MARKER}" ]
}

@test "the pinned fetch still resolves the merge-base on a clean config — the six pins change no fetch behavior" {
  make_fetch_repo
  run_fetch
  [ "$status" -eq 0 ]
  # the fetch's progress chatter lands in the same captured output;
  # the merge-base the function prints is the last line
  [ "${lines[${#lines[@]}-1]}" = "$BASE_SHA" ]
}

@test "model-crafted .git/hooks pre-*, post-* and reference-transaction scripts execute nothing on the fetch path" {
  # The round-9 payload, fetch-path leg: the same model-crafted hooks
  # review-restore.bats plants for the staging and restore paths are
  # planted here on the fetch fixture, and the fetch — git_authed's
  # invocation, whose pin set carries core.hooksPath=/dev/null — runs
  # straight past them. No command the fetch path runs consults a hook
  # even undefended (probed on git 2.43/2.54), so this pins the
  # posture: if a future git version fires one, this test fails before
  # the payload reaches production.
  make_fetch_repo
  plant_model_hooks
  run_fetch
  [ "$status" -eq 0 ]
  # no model-crafted hook executed
  [ ! -e "${TEST_TMP}/pre-commit-ran" ]
  [ ! -e "${TEST_TMP}/post-checkout-ran" ]
  [ ! -e "${TEST_TMP}/reference-transaction-ran" ]
  # and the fetch still resolved the merge-base
  [ "${lines[${#lines[@]}-1]}" = "$BASE_SHA" ]
}

@test "the fetch credential rides the env-numbered config channel — the assembled invocation and the ps view carry no token, the fetch still authenticates and resolves the merge-base" {
  # The round-9 Medium: the GitHub token must never ride the git
  # command line — a surviving model-spawned process reads any
  # process's arguments through ps. A git shim on PATH stands in for
  # the real git: it sees exactly the argv the library assembled and
  # exactly the environment the command-scoped assignments give it,
  # records both plus a ps-shaped view of its own process arguments
  # (the line a ps-reading observer would see), then execs the real
  # git — so the fetch genuinely runs under the env-numbered channel
  # and the merge-base still resolves. The local file:// fixture
  # cannot check an http credential server-side; the delivery proof is
  # the recorded channel state, and the clean-config test above proves
  # the fetch mechanics.
  make_fetch_repo
  STUB_TOKEN=stub-token-0123456789
  STUB_AUTH="AUTHORIZATION: basic $(printf 'x-access-token:%s' "${STUB_TOKEN}" | base64 | tr -d '\n')"
  SHIM_DIR="${TEST_TMP}/shim"
  RECORD="${TEST_TMP}/argv-record"
  REAL_GIT="$(command -v git)"
  mkdir -p "${SHIM_DIR}"
  {
    echo '#!/bin/sh'
    echo ': "${REAL_GIT:?git shim needs the real git path}"'
    echo ': "${RECORD:?git shim needs the record path}"'
    echo '{'
    echo '  printf "argv:%s\n" "$*"'
    echo '  printf "ps:%s\n" "$(ps -ww -o command= -p $$)"'
    echo '  printf "count:%s\n" "${GIT_CONFIG_COUNT:-unset}"'
    echo '  printf "key0:%s\n" "${GIT_CONFIG_KEY_0:-unset}"'
    echo '  printf "value0:%s\n" "${GIT_CONFIG_VALUE_0:-unset}"'
    echo '} >> "$RECORD"'
    echo 'exec "$REAL_GIT" "$@"'
  } > "${SHIM_DIR}/git"
  chmod +x "${SHIM_DIR}/git"
  : > "${RECORD}"
  run bash -c '
    set -euo pipefail
    cd "$1" || exit 1
    export REAL_GIT="$5" RECORD="$6"
    PATH="$7:$PATH"
    . "$2"
    fetch_review_base "$8" "$3" "$4"
  ' _ "${REPO_DIR}" "${LIB}" "${BASE_SHA}" "${HEAD_SHA}" "${REAL_GIT}" "${RECORD}" "${SHIM_DIR}" "${STUB_AUTH}"
  [ "$status" -eq 0 ]
  # the fetch still resolved the merge-base under the channel
  [ "${lines[${#lines[@]}-1]}" = "$BASE_SHA" ]
  # the assembled git invocation and the ps-shaped view of the process
  # arguments carry no credential: neither the raw token nor its
  # base64 form appears on any argv or ps line, and no extraheader
  # spelling rides the command line at all
  if grep -E '^(argv|ps):' "$RECORD" | grep -qF "${STUB_AUTH}" || grep -E '^(argv|ps):' "$RECORD" | grep -qF "${STUB_TOKEN}"; then
    echo "the fetch credential appears on the git command line or in the ps view" >&2
    return 1
  fi
  if grep -E '^(argv|ps):' "$RECORD" | grep -q extraheader; then
    echo "an extraheader rides the git command line instead of the env-numbered channel" >&2
    return 1
  fi
  # the channel delivered the credential to the fetch invocation: the
  # count is exactly 1, the key is the fixed literal, the value is the
  # auth the caller passed — this is the fetch authenticating
  grep -q '^count:1$' "$RECORD"
  grep -qF 'key0:http.https://github.com/.extraheader' "$RECORD"
  grep -qF "value0:${STUB_AUTH}" "$RECORD"
}
