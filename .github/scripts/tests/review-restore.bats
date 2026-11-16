# Behavioral tests for review-restore.sh — the sourced post-step
# restore-and-harden choreography of the opencode review lanes
# (opencode-review-reusable.yml's two post steps and the Enforce
# verdict step; ADR-0603). Each choreography element the helper owns
# is pinned here: PATH pin, git-configuration neutralization (the
# GIT_CONFIG_* source pins, the minimal trusted .git/config
# replacement, and the per-invocation fsmonitor/pager/worktree/
# ext-transport/attributesFile/hooksPath overrides), replace objects
# off (the export plus the reset's --no-replace-objects), the
# .git/info/attributes deletion, hooks off through the replacement
# itself (the round-9 payload: the minimal config carries
# core.hooksPath = /dev/null and a model-crafted .git/hooks pre-*,
# post-* or reference-transaction script never executes), the reset
# to the event-payload RESTORE_SHA (never the model-moveable local
# HEAD), the untracked sweep outside .stellar, findings-file
# rejection, the timeout marker read, and the --no-clean shape the
# enforce gate uses. The trusted-source staging the workflow wraps
# around every sourcing (git show of the PR head SHA into a fresh
# mktemp file) is pinned by the tamper fixture and the round-5/
# round-6/round-8/round-9 config payloads at the bottom: a model step
# with shell access can write .git/config, the worktree-scoped config
# and the shared HOME gitconfig, create refs/replace entries and
# .git/info/attributes, plant hook scripts in .git/hooks, and every
# git command the post steps run parses them.
#
# Local run:
#   bats .github/scripts/tests/review-restore.bats
#
# Requirements:
#   bats-core >= 1.5.0  (brew install bats-core)
#   git

bats_require_minimum_version 1.5.0

SCRIPT="$(cd "$(dirname "$BATS_TEST_FILENAME")/.." && pwd)/review-restore.sh"
PINNED_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
# The workflow-expression owner/name every step passes as REPO — the
# trusted source the helper's minimal replacement .git/config rebuilds
# the origin URL from. The URL is never fetched here; the slug only
# needs to be the real, well-formed shape.
REPO_SLUG=relatedb/relate

setup() {
  TEST_TMP="$(mktemp -d)"
  # Hermetic HOME/XDG: the config-payload tests below write a
  # model-written global gitconfig, and no test may touch the
  # developer's real one.
  export HOME="${TEST_TMP}/home"
  export XDG_CONFIG_HOME="${TEST_TMP}/xdg"
  mkdir -p "${HOME}" "${XDG_CONFIG_HOME}" "${TEST_TMP}/runner-tmp"
  # RUNNER_TEMP stands in for the runner's temp dir (the timeout
  # marker's channel); every test gets one, marker absent by default.
  export RUNNER_TEMP="${TEST_TMP}/runner-tmp"
  # REPO is part of the hermetic environment too: the helper requires
  # it (the minimal config's origin URL), and no test may read the
  # developer's real github context.
  export REPO="${REPO_SLUG}"
}

teardown() {
  rm -rf "${TEST_TMP}"
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# A scratch repo standing in for the runner checkout: one tracked file,
# an ignore rule for the droplet, and .stellar/ (ignored, as in the
# real repo) as the model-output channel. Exports RESTORE_SHA — the
# event-payload SHA every call site passes: the commit the checkout
# started from (the fixture's only commit, i.e. its HEAD at birth).
# The fixture path is REPO_DIR, never REPO: setup() exports REPO as the
# owner/name slug the helper's env requires, and an assignment to the
# exported name in the test shell would leak the path into the sourced
# helper's environment.
make_repo() {
  REPO_DIR="${TEST_TMP}/repo"
  mkdir -p "${REPO_DIR}/.stellar"
  git -C "${REPO_DIR}" init -q
  git -C "${REPO_DIR}" config user.email review-restore@test
  git -C "${REPO_DIR}" config user.name "review-restore test"
  echo base > "${REPO_DIR}/tracked.txt"
  printf 'droplet.txt\n.stellar/\n' > "${REPO_DIR}/.gitignore"
  git -C "${REPO_DIR}" add tracked.txt .gitignore
  git -C "${REPO_DIR}" commit -qm base
  RESTORE_SHA="$(git -C "${REPO_DIR}" rev-parse HEAD)"
  export RESTORE_SHA
}

# Source the helper the way the workflow steps do — inside the scratch
# repo, with the helper's arguments — and report TIMED_OUT and PATH
# from the sourcing shell afterwards.
run_helper() {
  run bash -c '
    cd "$1" || exit 1
    . "$2" "${@:3}"
    printf "TIMED_OUT=%s\n" "${TIMED_OUT:-unset}"
    printf "PATH=%s\n" "${PATH}"
  ' _ "${REPO_DIR}" "${SCRIPT}" "$@"
}

# A model-written "hook" script and its observable marker: the config
# payloads below point git's code-execution knobs (fsmonitor, pager,
# aliases, hooks) at it and assert the marker never appears.
plant_marker_hook() {
  EVIL="${TEST_TMP}/model-payload.sh"
  MARKER="${TEST_TMP}/payload-ran"
  printf '#!/bin/sh\ntouch "%s"\n' "${MARKER}" > "${EVIL}"
  chmod +x "${EVIL}"
}

# The round-9 payload: real hook scripts a model shell plants in the
# default .git/hooks directory — a pre-*, a post-* and the
# reference-transaction an undefended `git reset --hard` executes
# (probed on git 2.43/2.54, even for a reset that lands where the ref
# already points; no pre-*/post-* hook fires on any command the
# restore, staging and fetch paths run today, but the posture must
# hold for every hook the directory offers). Each writes its own
# marker; the tests below assert none ever appears.
plant_model_hooks() {
  printf '#!/bin/sh\ntouch "%s"\n' "${TEST_TMP}/pre-commit-ran" > "${REPO_DIR}/.git/hooks/pre-commit"
  printf '#!/bin/sh\ntouch "%s"\n' "${TEST_TMP}/post-checkout-ran" > "${REPO_DIR}/.git/hooks/post-checkout"
  printf '#!/bin/sh\ntouch "%s"\n' "${TEST_TMP}/reference-transaction-ran" > "${REPO_DIR}/.git/hooks/reference-transaction"
  chmod +x "${REPO_DIR}/.git/hooks/pre-commit" "${REPO_DIR}/.git/hooks/post-checkout" "${REPO_DIR}/.git/hooks/reference-transaction"
}

# The trusted-source fixture: a stand-in runner checkout whose one
# commit — the event-payload SHA the workflow stages and resets from —
# carries the real helper at .github/scripts/review-restore.sh. The
# tamper and config-payload tests below plant model-step output on top
# of it. Exports PR_HEAD (the stand-in head SHA) and RESTORE_SHA.
make_helper_repo() {
  REPO_DIR="${TEST_TMP}/repo"
  mkdir -p "${REPO_DIR}/.github/scripts" "${REPO_DIR}/.stellar"
  git -C "${REPO_DIR}" init -q
  git -C "${REPO_DIR}" config user.email review-restore@test
  git -C "${REPO_DIR}" config user.name "review-restore test"
  cp "${SCRIPT}" "${REPO_DIR}/.github/scripts/review-restore.sh"
  echo base > "${REPO_DIR}/tracked.txt"
  printf 'droplet.txt\n.stellar/\n' > "${REPO_DIR}/.gitignore"
  git -C "${REPO_DIR}" add .github/scripts/review-restore.sh tracked.txt .gitignore
  git -C "${REPO_DIR}" commit -qm base
  PR_HEAD="$(git -C "${REPO_DIR}" rev-parse HEAD)"
  RESTORE_SHA="$PR_HEAD"
  export RESTORE_SHA
}

# The workflow's trusted-source path (opencode-review-reusable.yml's
# post steps): stage the committed PR-head copy of the helper with the
# pinned git invocation — the workflow's exact spelling,
# --no-replace-objects plus the six config pins — verify the staged
# bytes are the trusted commit's, then source the staged copy — all
# inside a stand-in for the post step's shell, so the helper's
# exported pins land there.
run_trusted_staging() {
  run bash -c '
    cd "$1" || exit 1
    RESTORE="$(mktemp)"
    git --no-replace-objects -c core.fsmonitor= -c core.pager=cat -c core.worktree=. -c protocol.ext.allow=never -c core.attributesFile= -c core.hooksPath=/dev/null show "$2:.github/scripts/review-restore.sh" > "$RESTORE"
    if cmp -s "$RESTORE" "$3"; then
      printf "STAGED_TRUSTED=yes\n"
    else
      printf "STAGED_TRUSTED=no\n"
    fi
    . "$RESTORE" "${@:4}"
    printf "TIMED_OUT=%s\n" "${TIMED_OUT:-unset}"
    printf "PATH=%s\n" "${PATH}"
  ' _ "${REPO_DIR}" "${PR_HEAD}" "${SCRIPT}" "$@"
}

# ---------------------------------------------------------------------------
# Interface guards
# ---------------------------------------------------------------------------

@test "sourcing without arguments fails loudly (a bare source would inherit the caller's positionals)" {
  make_repo
  run bash -c '
    helper=$1
    shift
    . "$helper"
    echo "should not reach here"
  ' _ "${SCRIPT}"
  [ "$status" -ne 0 ]
  [[ "$output" != *"should not reach here"* ]]
  [[ "$output" == *"source it with --no-clean or the findings file list"* ]]
}

@test "an unknown flag fails loudly instead of being treated as a findings file" {
  make_repo
  run_helper --bogus
  [ "$status" -ne 0 ]
  [[ "$output" == *"unknown flag: --bogus"* ]]
}

# ---------------------------------------------------------------------------
# Findings-file rejection
# ---------------------------------------------------------------------------

@test "a findings symlink is deleted and reported, its target untouched" {
  make_repo
  secret="${TEST_TMP}/runner-secret"
  echo token > "${secret}"
  ln -s "${secret}" "${REPO_DIR}/.stellar/findings.json"
  run_helper .stellar/findings.json
  [ "$status" -eq 0 ]
  [[ "$output" == *"rejecting non-model output .stellar/findings.json (symlink or tracked at HEAD) — deleting"* ]]
  [ ! -e "${REPO_DIR}/.stellar/findings.json" ]
  [ -f "${secret}" ]
}

@test "a model-created FIFO at the findings path is rejected without ever being opened — the post step cannot stall on it" {
  # A FIFO is the cheapest non-regular file the model can plant: the
  # poster's open() would block until a writer appears, stalling the
  # post step. The gate must delete it on sight — stat only, never an
  # open — so reaching the assertions below at all proves the helper
  # did not hang on it.
  make_repo
  mkfifo "${REPO_DIR}/.stellar/findings.json"
  run_helper .stellar/findings.json
  [ "$status" -eq 0 ]
  [[ "$output" == *"rejecting non-model output .stellar/findings.json (symlink or tracked at HEAD) — deleting"* ]]
  [ ! -e "${REPO_DIR}/.stellar/findings.json" ]
}

@test "a findings file force-added into the merge ref survives the reset and is still rejected" {
  # The poster inline comment's threat (OPLANE_REQ-00333782): a PR
  # force-adds .stellar/findings.json into its merge ref, so the event
  # payload's SHA — the RESTORE_SHA the reset targets — points at a
  # commit that tracks the planted file. It genuinely survives the
  # reset and would pose as model output, steering the poster onto
  # the pass-capable branch; the fixture mirrors that world by
  # re-pointing RESTORE_SHA at the force-adding commit.
  make_repo
  echo '{"forced": true}' > "${REPO_DIR}/.stellar/findings.json"
  git -C "${REPO_DIR}" add -f .stellar/findings.json
  git -C "${REPO_DIR}" commit -qm "force-add findings"
  RESTORE_SHA="$(git -C "${REPO_DIR}" rev-parse HEAD)"
  export RESTORE_SHA
  run_helper .stellar/findings.json
  [ "$status" -eq 0 ]
  [[ "$output" == *"rejecting non-model output .stellar/findings.json (symlink or tracked at HEAD) — deleting"* ]]
  [ ! -e "${REPO_DIR}/.stellar/findings.json" ]
}

# ---------------------------------------------------------------------------
# Reset and clean
# ---------------------------------------------------------------------------

@test "staged and unstaged edits reset to the trusted RESTORE_SHA; untracked and ignored shadows outside .stellar removed, .stellar kept" {
  make_repo
  # the model can edit AND stage — checkout -- would restore from the index
  echo tampered > "${REPO_DIR}/tracked.txt"
  git -C "${REPO_DIR}" add tracked.txt
  echo shadow > "${REPO_DIR}/shadow.txt"
  echo droplet > "${REPO_DIR}/droplet.txt"
  echo keep > "${REPO_DIR}/.stellar/keep.txt"
  echo partial > "${REPO_DIR}/.stellar/findings.partial.json"
  run_helper .stellar/findings.json .stellar/findings.partial.json
  [ "$status" -eq 0 ]
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  [ -z "$(git -C "${REPO_DIR}" status --porcelain)" ]
  [ ! -e "${REPO_DIR}/shadow.txt" ]
  [ ! -e "${REPO_DIR}/droplet.txt" ]
  [ -f "${REPO_DIR}/.stellar/keep.txt" ]
  # a regular untracked findings file is model output, not a shadow: kept
  [ -f "${REPO_DIR}/.stellar/findings.partial.json" ]
}

@test "--no-clean keeps untracked files outside .stellar while the reset still runs (the enforce gate's verdict file)" {
  make_repo
  echo tampered > "${REPO_DIR}/tracked.txt"
  echo pass > "${REPO_DIR}/.opencode-verdict.txt"
  echo shadow > "${REPO_DIR}/shadow.txt"
  run_helper --no-clean
  [ "$status" -eq 0 ]
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  [ -f "${REPO_DIR}/.opencode-verdict.txt" ]
  [ -f "${REPO_DIR}/shadow.txt" ]
}

# ---------------------------------------------------------------------------
# Trusted reset target
# ---------------------------------------------------------------------------

@test "sourcing without RESTORE_SHA fails loudly — no fallback to local HEAD, which the model can move" {
  make_repo
  RESTORE_SHA= run_helper .stellar/findings.json
  [ "$status" -ne 0 ]
  [[ "$output" == *"RESTORE_SHA required"* ]]
}

@test "the reset targets RESTORE_SHA, never a locally moved HEAD (the model can commit and move HEAD)" {
  make_repo
  # the model commits its tampered tree and moves HEAD: a reset to HEAD
  # would restore the model's tree instead of the reviewed one
  echo moved > "${REPO_DIR}/tracked.txt"
  git -C "${REPO_DIR}" add tracked.txt
  git -C "${REPO_DIR}" commit -qm "model commit moves HEAD"
  [ "$(git -C "${REPO_DIR}" rev-parse HEAD)" != "$RESTORE_SHA" ]
  run_helper .stellar/findings.json
  [ "$status" -eq 0 ]
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  [ "$(git -C "${REPO_DIR}" rev-parse HEAD)" = "$RESTORE_SHA" ]
}

# ---------------------------------------------------------------------------
# Trusted-source staging (the workflow's sourcing path)
# ---------------------------------------------------------------------------

@test "a model-rewritten worktree copy of the helper cannot alter what a token-bearing step executes — the trusted source wins" {
  # The tamper fixture: the PR head commit (the stand-in for the
  # event payload's head SHA) holds the real helper at
  # .github/scripts/review-restore.sh, and the model step has
  # rewritten the worktree copy. The workflow path stages the
  # committed copy via git show into a fresh mktemp file and sources
  # that — the worktree copy is never consulted.
  make_helper_repo
  # the model step rewrites the worktree copy of the helper (and the
  # tracked file, to prove the real choreography really ran) — the
  # tamper is maximally observable: a stderr banner, a marker file, a
  # poisoned PATH, and no reset
  {
    echo 'echo "TAMPER_RAN" >&2'
    echo 'touch .tamper-ran'
    echo 'export PATH=/tmp/model-bin:$PATH'
    echo 'return 0'
  } > "${REPO_DIR}/.github/scripts/review-restore.sh"
  echo tampered > "${REPO_DIR}/tracked.txt"
  # the workflow's path: stage the committed PR-head copy, source it
  run_trusted_staging .stellar/findings.json
  [ "$status" -eq 0 ]
  # the trusted copy ran: the reset restored the reviewed tree, the
  # PATH pin and the timeout-marker read reached the sourcing shell
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  [[ "$output" == *"PATH=${PINNED_PATH}"* ]]
  [[ "$output" == *"TIMED_OUT=0"* ]]
  # the tampered copy never executed, and its rewrite did not survive
  [[ "$output" != *"TAMPER_RAN"* ]]
  [ ! -e "${REPO_DIR}/.tamper-ran" ]
  cmp -s "${SCRIPT}" "${REPO_DIR}/.github/scripts/review-restore.sh"
}

# ---------------------------------------------------------------------------
# Model-written git configuration (the round-5 and round-6 findings and the
# round-4 model experiments: the model step can write .git/config with
# core.fsmonitor, core.worktree, filters and aliases, and the shared HOME
# gitconfig — and every git command the post steps run parses that config)
# ---------------------------------------------------------------------------

@test "a model-written .git/config core.fsmonitor executes nothing — the restore's git calls pin fsmonitor and pager off per invocation" {
  make_helper_repo
  plant_marker_hook
  # the round-5 payload: core.fsmonitor (and a pager) in .git/config
  # pointing at a marker script — without the per-invocation -c
  # overrides, every index-refreshing git command in the restore
  # (reset --hard, clean, ls-files) runs the model's fsmonitor hook
  # with the write token
  git -C "${REPO_DIR}" config core.fsmonitor "${EVIL}"
  git -C "${REPO_DIR}" config core.pager "${EVIL}"
  echo tampered > "${REPO_DIR}/tracked.txt"
  run_trusted_staging .stellar/findings.json
  [ "$status" -eq 0 ]
  # no model-written fsmonitor or pager ever executed
  [ ! -e "${MARKER}" ]
  # the restore itself still ran: tracked content back to the trusted base
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  # the staged helper is the trusted commit's bytes, not a steered read
  [[ "$output" == *"STAGED_TRUSTED=yes"* ]]
}

@test "a model-written global gitconfig (the shared HOME) is ignored, not obeyed or edited — GIT_CONFIG_GLOBAL pins it to /dev/null" {
  make_helper_repo
  plant_marker_hook
  # the round-4 experiment payload: the model shell wrote the shared
  # HOME gitconfig — fsmonitor, a pager and a hooks dir of its own
  mkdir -p "${TEST_TMP}/model-githooks"
  printf '#!/bin/sh\ntouch "%s"\n' "${MARKER}" > "${TEST_TMP}/model-githooks/post-checkout"
  chmod +x "${TEST_TMP}/model-githooks/post-checkout"
  {
    echo "[core]"
    echo "	fsmonitor = ${EVIL}"
    echo "	pager = ${EVIL}"
    echo "	hooksPath = ${TEST_TMP}/model-githooks"
  } > "${HOME}/.gitconfig"
  echo tampered > "${REPO_DIR}/tracked.txt"
  run_trusted_staging .stellar/findings.json
  [ "$status" -eq 0 ]
  # no model-written global fsmonitor, pager or hook ever executed
  [ ! -e "${MARKER}" ]
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  [[ "$output" == *"STAGED_TRUSTED=yes"* ]]
  # the model-written global gitconfig is untouched — ignored under
  # the GIT_CONFIG_GLOBAL pin, never rewritten
  grep -q "hooksPath = ${TEST_TMP}/model-githooks" "${HOME}/.gitconfig"
}

@test "a planted object-store alias attempt stays inert — builtins win, the trusted SHA still resolves to the trusted bytes" {
  make_helper_repo
  plant_marker_hook
  # the model step's redirect attempts on the object-store read the
  # post steps stage from: config aliases shadowing the four git
  # commands of the restore path, and an alternates entry aliasing a
  # model-controlled directory into the object store
  git -C "${REPO_DIR}" config alias.show "!${EVIL}"
  git -C "${REPO_DIR}" config alias.reset "!${EVIL}"
  git -C "${REPO_DIR}" config alias.clean "!${EVIL}"
  git -C "${REPO_DIR}" config alias.ls-files "!${EVIL}"
  mkdir -p "${TEST_TMP}/model-objects" "${REPO_DIR}/.git/objects/info"
  echo "${TEST_TMP}/model-objects" > "${REPO_DIR}/.git/objects/info/alternates"
  echo tampered > "${REPO_DIR}/tracked.txt"
  run_trusted_staging .stellar/findings.json
  [ "$status" -eq 0 ]
  # no alias ever executed: git builtins take precedence over alias
  # definitions, so the planted names never run
  [ ! -e "${MARKER}" ]
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  # the staged helper came from the real git show builtin reading the
  # event-payload SHA — the trusted-commit bytes, not an alias's
  # output; an object-store write the model can perform cannot
  # redirect a full-SHA object read
  [[ "$output" == *"STAGED_TRUSTED=yes"* ]]
}

@test "a model-written core.worktree decoy redirect leaves the real worktree restored — the trusted config replacement wins" {
  make_helper_repo
  DECOY="${TEST_TMP}/decoy"
  mkdir -p "${DECOY}"
  # the round-6 payload: core.worktree in .git/config redirects every
  # worktree-touching git call to a decoy directory — without a defense,
  # reset --hard restores the DECOY and leaves the real worktree
  # tampered (probed on git 2.43: the reset's tree lands in the decoy),
  # so the poster would keep reading model-rewritten scripts. The
  # per-invocation -c core.worktree=. pin cannot override it (git's
  # setup ignores command-line core.worktree — probed on 2.43 and
  # 2.54), which is exactly why the choreography replaces the config
  # file outright before its first git invocation.
  git -C "${REPO_DIR}" config core.worktree "${DECOY}"
  echo tampered > "${REPO_DIR}/tracked.txt"
  run_trusted_staging .stellar/findings.json
  [ "$status" -eq 0 ]
  # the real worktree is restored to the trusted base, not the decoy
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  # the decoy directory never received the reset
  [ -z "$(ls -A "${DECOY}")" ]
  # the staged helper is the trusted commit's bytes, not a steered read
  [[ "$output" == *"STAGED_TRUSTED=yes"* ]]
}

@test "a model-written filter.<driver>.smudge plus core.attributesFile pair executes nothing during the reset" {
  make_helper_repo
  plant_marker_hook
  # the round-6 payload: a smudge filter defined in .git/config, bound
  # to every path by a model-written attributes file the config points
  # at — without the config replacement the filter command runs during
  # reset --hard's checkout (probed on git 2.43: the marker appears and
  # the reset still completes). The payload binds the filter through
  # BOTH attribute sources: core.attributesFile (the per-invocation
  # -c core.attributesFile= pin holds that one closed even in a shell
  # where the replacement has not run) and a model-written worktree
  # .gitattributes (only the replacement closes that one — with the
  # filter definition gone from the config, no binding can execute).
  git -C "${REPO_DIR}" config filter.evil.smudge "${EVIL}"
  printf '* filter=evil\n' > "${TEST_TMP}/model-attrs"
  git -C "${REPO_DIR}" config core.attributesFile "${TEST_TMP}/model-attrs"
  printf 'tracked.txt filter=evil\n' > "${REPO_DIR}/.gitattributes"
  echo tampered > "${REPO_DIR}/tracked.txt"
  run_trusted_staging .stellar/findings.json
  [ "$status" -eq 0 ]
  # no model-written filter ever executed
  [ ! -e "${MARKER}" ]
  # the restore itself still ran: tracked content back to the trusted base
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  [[ "$output" == *"STAGED_TRUSTED=yes"* ]]
}

@test "the repository config is replaced with the minimal trusted configuration before any git call runs" {
  make_helper_repo
  # plant exactly the payload keys the replacement must drop: a decoy
  # hooks path, a decoy worktree, a smudge filter, an insteadOf
  # rewrite, and the worktreeConfig extension that would make a
  # planted .git/config.worktree parse as configuration
  mkdir -p "${REPO_DIR}/.githooks"
  git -C "${REPO_DIR}" config core.hooksPath .githooks
  git -C "${REPO_DIR}" config core.worktree "${TEST_TMP}/decoy"
  git -C "${REPO_DIR}" config filter.evil.smudge "${TEST_TMP}/evil.sh"
  git -C "${REPO_DIR}" config url.ext::evil.insteadOf https://github.com/
  git -C "${REPO_DIR}" config extensions.worktreeConfig true
  printf '[core]\n\thooksPath = .githooks\n' > "${REPO_DIR}/.git/config.worktree"
  run_helper .stellar/findings.json
  [ "$status" -eq 0 ]
  # the file is exactly the minimal trusted configuration — the core
  # block git init writes, the hooksPath neutralization the round-9
  # finding added, plus the origin remote rebuilt from the trusted REPO
  # slug — nothing sourced from model-writable state
  [ "$(cat "${REPO_DIR}/.git/config")" = "$(printf '[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = false\n\tlogallrefupdates = true\n\thooksPath = /dev/null\n[remote "origin"]\n\turl = https://github.com/%s\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n' "${REPO_SLUG}")" ]
  # the planted decoy hooks path is gone with the whole model-writable
  # file: the one deliberate hooksPath is the neutralizing one
  [ "$(git -C "${REPO_DIR}" config --local --get core.hooksPath)" = "/dev/null" ]
  # no other planted key survives in the repository config: git
  # resolves none of them (run/[ status ] — a `!`-prefixed command
  # never fails a bats test), and the planted worktree-scoped config
  # file is inert without the extension
  for key in core.worktree filter.evil.smudge url.ext::evil.insteadOf extensions.worktreeConfig; do
    run git -C "${REPO_DIR}" config --local --get "$key"
    [ "$status" -ne 0 ]
  done
  # and the checkout still works under it: the reset ran clean
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
}

@test "a model-created refs/replace entry for RESTORE_SHA cannot steer the staged helper bytes or the reset target — replace objects are off" {
  make_helper_repo
  # the round-8 payload: during the model step the attacker crafts a
  # commit — a helper that prints a banner, touches a marker and
  # skips the choreography, plus a tampered tracked file — and points
  # refs/replace/<RESTORE_SHA> at it. Without --no-replace-objects,
  # git resolves RESTORE_SHA through the replacement: the staging git
  # show would stage the attacker's helper bytes (and the sourcing
  # shell would run them), and the reset would land on the attacker's
  # tree instead of the reviewed one.
  {
    echo 'echo "REPLACE_STEER_RAN" >&2'
    echo 'touch .replace-ran'
    echo 'return 0'
  } > "${REPO_DIR}/.github/scripts/review-restore.sh"
  echo replaced > "${REPO_DIR}/tracked.txt"
  git -C "${REPO_DIR}" add -A
  git -C "${REPO_DIR}" commit -qm "attacker-crafted replacement commit"
  ATTACKER_SHA="$(git -C "${REPO_DIR}" rev-parse HEAD)"
  git -C "${REPO_DIR}" update-ref "refs/replace/${RESTORE_SHA}" "${ATTACKER_SHA}"
  # the worktree stays as the model step left it — tampered helper,
  # tampered tracked file; the planted replace ref is the payload's
  # steering channel, and the staged choreography must undo the rest
  run_trusted_staging .stellar/findings.json
  [ "$status" -eq 0 ]
  # the staged bytes are the true object's, not the replacement's
  [[ "$output" == *"STAGED_TRUSTED=yes"* ]]
  # the reset landed on the true tree: the attacker's tracked-file
  # content is gone and the branch is back at the trusted commit
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  [ "$(git -C "${REPO_DIR}" rev-parse HEAD)" = "${RESTORE_SHA}" ]
  # the attacker's helper never executed
  [[ "$output" != *"REPLACE_STEER_RAN"* ]]
  [ ! -e "${REPO_DIR}/.replace-ran" ]
}

@test "a model-written .git/info/attributes filter binding executes nothing — the choreography deletes the file before its first git call" {
  make_helper_repo
  plant_marker_hook
  # the round-8 payload: the filter definition in .git/config plus the
  # binding in .git/info/attributes — the one attribute source no pin
  # or replacement covered. The config replacement deletes the
  # definition, but a persistent model-spawned child can re-add one to
  # .git/config between the choreography's git calls; the binding
  # source must not survive either. The marker script writes nothing
  # to stdout, so an undefended smudge would also empty the file —
  # the reset must write the raw blob bytes.
  git -C "${REPO_DIR}" config filter.evil.smudge "${EVIL}"
  mkdir -p "${REPO_DIR}/.git/info"
  printf 'tracked.txt filter=evil\n' > "${REPO_DIR}/.git/info/attributes"
  echo tampered > "${REPO_DIR}/tracked.txt"
  run_trusted_staging .stellar/findings.json
  [ "$status" -eq 0 ]
  # no model-written filter executed during the reset
  [ ! -e "${MARKER}" ]
  # the reset wrote the raw blob bytes, not a filter's output
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  [[ "$output" == *"STAGED_TRUSTED=yes"* ]]
  # the binding source is deleted alongside the config replacement —
  # no git call later in the step can parse it
  [ ! -e "${REPO_DIR}/.git/info/attributes" ]
}

@test "sourcing without REPO fails loudly — the minimal config's origin URL is never read back from model-writable state" {
  make_repo
  run bash -c '
    cd "$1" || exit 1
    unset REPO
    . "$2" .stellar/findings.json
    echo "should not reach here"
  ' _ "${REPO_DIR}" "${SCRIPT}"
  [ "$status" -ne 0 ]
  [[ "$output" != *"should not reach here"* ]]
  [[ "$output" == *"REPO required"* ]]
}

# ---------------------------------------------------------------------------
# Timeout marker read
# ---------------------------------------------------------------------------

@test "TIMED_OUT is 1 when the run step's marker says so" {
  make_repo
  printf 1 > "${RUNNER_TEMP}/review_timed_out"
  run_helper .stellar/findings.json
  [ "$status" -eq 0 ]
  [[ "$output" == *"TIMED_OUT=1"* ]]
}

@test "a missing marker means no timeout: TIMED_OUT is 0" {
  make_repo
  run_helper .stellar/findings.json
  [ "$status" -eq 0 ]
  [[ "$output" == *"TIMED_OUT=0"* ]]
}

# ---------------------------------------------------------------------------
# PATH pin and hooks neutralization
# ---------------------------------------------------------------------------

@test "the PATH pin reaches the sourcing shell" {
  make_repo
  run_helper .stellar/findings.json
  [ "$status" -eq 0 ]
  [[ "$output" == *"PATH=${PINNED_PATH}"* ]]
}

@test "hooks are neutralized: the replaced repository config pins hooksPath to /dev/null, the global config pinned away" {
  make_repo
  # the model step planted a local hooks path AND wrote a global
  # gitconfig naming its own hooks dir
  git -C "${REPO_DIR}" config core.hooksPath .githooks
  mkdir -p "${TEST_TMP}/model-githooks"
  printf '#!/bin/sh\nexit 0\n' > "${TEST_TMP}/model-githooks/post-checkout"
  git config --global core.hooksPath "${TEST_TMP}/model-githooks"
  run_helper .stellar/findings.json
  [ "$status" -eq 0 ]
  # the planted local hooksPath is gone with the whole model-writable
  # file, replaced by the neutralizing one: the minimal replacement
  # config carries hooksPath = /dev/null, so a --local --get resolves
  # exactly that — every hook lookup lands on /dev/null, where none is
  # found. (--local scopes away the planted global below, which --get
  # alone would keep finding.)
  [ "$(git -C "${REPO_DIR}" config --local --get core.hooksPath)" = "/dev/null" ]
  # the model-written global gitconfig is untouched — ignored under
  # the GIT_CONFIG_GLOBAL pin, never rewritten
  grep -q "hooksPath = ${TEST_TMP}/model-githooks" "${HOME}/.gitconfig"
}

@test "model-crafted .git/hooks pre-*, post-* and reference-transaction scripts execute nothing across the staging and restore paths" {
  # The round-9 payload: the model shell writes real hook scripts into
  # the default .git/hooks directory — the one hooks source no round-5
  # through round-8 pin covered, whose guarantee the round-7
  # GIT_CONFIG_GLOBAL=/dev/null pin had masked since it dropped the
  # global hooksPath file. Probed on git 2.43/2.54: an undefended
  # `git reset --hard` executes a model-crafted
  # .git/hooks/reference-transaction (even a reset that lands where
  # the ref already points); the pre-* and post-* scripts fire on no
  # command these paths run today, but they are planted too — the
  # posture must hold for every hook the directory offers. The defense
  # is the config replacement itself: the minimal trusted .git/config
  # carries core.hooksPath = /dev/null, the staging show and every git
  # invocation the choreography runs pin the same per invocation, and
  # every hook lookup lands on /dev/null, where none is found.
  make_helper_repo
  # the model moves HEAD with a commit (the reset's ref update is
  # real), then plants the hooks — the markers prove none executed
  echo moved > "${REPO_DIR}/tracked.txt"
  git -C "${REPO_DIR}" add tracked.txt
  git -C "${REPO_DIR}" commit -qm "model commit moves HEAD"
  plant_model_hooks
  run_trusted_staging .stellar/findings.json
  [ "$status" -eq 0 ]
  # no model-crafted hook executed — not the pre-*, not the post-*,
  # not the reference-transaction the reset would otherwise run
  [ ! -e "${TEST_TMP}/pre-commit-ran" ]
  [ ! -e "${TEST_TMP}/post-checkout-ran" ]
  [ ! -e "${TEST_TMP}/reference-transaction-ran" ]
  # the restore still completed: the tracked file is back at the
  # trusted base and the staged helper was the trusted commit's bytes
  [ "$(cat "${REPO_DIR}/tracked.txt")" = "base" ]
  [[ "$output" == *"STAGED_TRUSTED=yes"* ]]
}
