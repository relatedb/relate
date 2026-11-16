# review-restore.sh — the one restore-and-harden choreography every
# step runs after the shell-capable model step (ADR-0603): the scripted
# stellar post, the per-kind posts and the Enforce verdict gate all
# re-enter the workspace through this script. Sourced, never executed —
# the PATH and git-config pins must reach the calling step's own
# commands (python3, gh, jq, the poster scripts, its git calls), and a
# child process cannot export them back.
#
#   . .github/scripts/review-restore.sh [--no-clean] <findings-file>...
#   env: RESTORE_SHA — the event-payload SHA the reset targets
#        (required; the caller passes the merge SHA github.sha)
#        REPO — the trusted owner/name (github.repository) the minimal
#        replacement .git/config rebuilds the origin URL from
#        (required; never read back from the model-writable config)
#
# Always source it with an explicit argument list: a bare `.` source
# inherits the caller's positional parameters.
#
# Source it from trusted content, never the worktree copy: the calling
# step stages the committed PR-head copy — git show of the head SHA the
# event payload carries — into a fresh mktemp file and sources that,
# because the model step could have rewritten any worktree file, this
# one included, before a token-bearing step runs.
#
# The model step runs with shell access, so nothing it left behind is
# trusted:
#   - PATH is pinned to the system directories: the model step could
#     have poisoned later steps' PATH through $GITHUB_PATH.
#   - Git configuration is neutralized at every source the model shell
#     can write: the global (shared HOME) and system gitconfigs are
#     pinned to /dev/null and the env-numbered config channel is
#     zeroed (the GIT_CONFIG_* exports below), while the repository's
#     own .git/config — the one source those pins leave parsing — is
#     replaced outright with the minimal trusted configuration before
#     the first git invocation (nothing in it is sourced from
#     model-writable state), and every git invocation the choreography
#     itself performs also pins the repository config's remaining
#     knobs off per invocation: -c core.fsmonitor= -c core.pager=cat
#     -c core.worktree=. -c protocol.ext.allow=never
#     -c core.attributesFile= -c core.hooksPath=/dev/null (the
#     round-6 payloads: a model-written core.worktree redirecting the
#     reset to a decoy directory, a filter.<driver>.smudge plus
#     core.attributesFile pair executing during reset --hard, and a
#     url.<url>.insteadOf rewrite of the fetch URL to an ext::
#     command — probed on git 2.43/2.54: the replacement is the
#     load-bearing defense for core.worktree, which git's setup
#     ignores -c overrides for; the pins hold the other knobs closed
#     even where the replacement has not run; the round-9 payload:
#     a model-crafted .git/hooks script must never execute — the
#     hooksPath pin redirects every hook lookup to /dev/null, where
#     none is found).
#   - Replace objects are off: the model step can create
#     refs/replace/<SHA> entries, and git resolves every SHA through
#     them by default. GIT_NO_REPLACE_OBJECTS is exported below before
#     the first git call, and the reset itself carries
#     --no-replace-objects on the command line (the staging git show
#     that sourced this script carries it too) — a model-created
#     replacement cannot steer which tree the reset lands on.
#   - .git/info/attributes is deleted outright before the first git
#     call, alongside the config replacement (the round-8 finding):
#     it is model-writable, parses as an attribute source in every
#     git call — above the worktree's own .gitattributes — and no pin
#     or replacement covers it. With it gone, the only attribute
#     sources left are the trusted worktree .gitattributes (which the
#     reset restores) and the pinned-empty core.attributesFile, so no
#     model-written filter or driver binding survives into any git
#     call — even one a persistent model-spawned child re-arms by
#     rewriting .git/config mid-step.
#   - Hooks are neutralized before any git command runs: the config
#     replacement writes core.hooksPath = /dev/null into the minimal
#     trusted repository configuration, and every git invocation the
#     choreography performs also pins it per invocation — the model
#     shell can write the default .git/hooks directory (the round-9
#     finding: probed on git 2.43/2.54, an undefended reset --hard
#     executes a model-crafted .git/hooks/reference-transaction, and
#     the round-7 GIT_CONFIG_GLOBAL=/dev/null pin had been masking the
#     round-6 hooks guarantee since it dropped the global hooksPath
#     file), so that directory must never be consulted: every hook
#     lookup lands on /dev/null, where none is found. The
#     global-config pin above keeps a model-written global hooksPath
#     from applying to a git command in the step too.
#   - Tracked files are reset to RESTORE_SHA, the event-payload SHA of
#     the commit the checkout started from — reset --hard, not
#     checkout --: the model can stage files, and checkout restores
#     from the INDEX; and never local HEAD: the model can commit and
#     move it, so only the event payload's SHA is trusted.
#   - Untracked shadow files outside .stellar are removed, ignored ones
#     included (-x: a gitignored droplet must not survive either, and a
#     planted .opencode-verdict.txt must NOT survive — the poster
#     writes it). .stellar is the model-output channel and survives.
#   - Every findings file named as an argument is accepted only as a
#     REGULAR, UNTRACKED file: a symlink (pointing anywhere, runner
#     files included), a file tracked at HEAD, or any other
#     non-regular file (a model-created FIFO, socket or device at a
#     findings path would stall the poster's read) is deleted before
#     validation and treated as no output — a PR force-adding
#     .stellar/findings.json would survive the reset and pose as model
#     output, steering the poster onto the pass-capable branch
#     (OPLANE_REQ-00333782). The model cannot place a tracked file
#     here: its commits never reach this checkout's HEAD.
#   - The timeout fact is read from the run step's marker file in
#     $RUNNER_TEMP — written after the model process exited, never an
#     env var the model could have forged into $GITHUB_ENV
#     (OPLANE_REQ-00333784) — and left in TIMED_OUT for the caller: 1
#     when the run timed out, 0 when it did not. A missing marker
#     means the run step never reached its wrapper write: no timeout
#     salvage, the generic fail path decides.
#
# --no-clean skips the untracked-file sweep alone (PATH and git-config
# pins, hooks and reset still run): the Enforce verdict gate takes the
# choreography that way because its verdict file is untracked and must
# survive.
set -euo pipefail

NO_CLEAN=0
if [ "${1:-}" = "--no-clean" ]; then
  NO_CLEAN=1
  shift
fi
if [ "$NO_CLEAN" -ne 1 ] && [ "$#" -eq 0 ]; then
  echo "review-restore.sh: source it with --no-clean or the findings file list as arguments" >&2
  return 1 2>/dev/null || exit 1
fi
for arg in "$@"; do
  case "$arg" in
    --*)
      echo "review-restore.sh: unknown flag: $arg" >&2
      return 1 2>/dev/null || exit 1
      ;;
  esac
done

export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
# Model-written git configuration is inert from the first git call in
# this step on (the round-5 finding: the model shell can write
# .git/config and the shared HOME gitconfig, and every git command the
# post steps run parses them): the global and system gitconfigs are
# pinned to /dev/null, and the env-numbered config channel —
# GIT_CONFIG_KEY_n/GIT_CONFIG_VALUE_n pairs, plantable into this
# step's environment through the runner env file under RUNNER_TEMP —
# is zeroed by GIT_CONFIG_COUNT=0. Exported like the PATH pin, and for
# the same reason: the calling step's own later git calls must run
# under them, and a child process cannot export them back. The
# workflow's step env pins the same values from process start
# (ADR-0603), covering the staging git show that runs before this
# script is sourced. The zero is the posture of every git call but the
# authenticated fetches: lib/review-endpoints.sh's git_authed (sourced
# by the prep scripts this choreography precedes) raises the count to
# exactly 1 through command-scoped assignments of a fixed key and the
# step's assembled credential, for the fetch command alone — the two
# cannot conflict, and a model-published pair stays inert everywhere
# else.
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_COUNT=0
# GIT_COMMON_DIR and its siblings redirect where git finds the common
# directory — config, refs, the object store — so a model-published
# value serves its own config to every git call in this step, bypassing
# the replaced .git/config entirely (the round-16 finding: a smudge
# filter defined in a redirected common dir executes during the reset).
# Unset here as well as in the workflow's step blocks: defense for
# contexts that source this script without them.
unset GIT_COMMON_DIR GIT_NAMESPACE GIT_SHALLOW_FILE GIT_REPLACE_REF_BASE
# The runner's real HOME is shared with the model step (same user), so
# every HOME-adjacent config the token-bearing commands consult is
# model-writable: gh's hosts.yml (endpoint/credential redirect), the
# Python user site (import hijack), and jq's HOME-resident state. The
# choreography re-homes into a fresh directory from its first line on
# (the round-13 finding): gh gets an empty config dir and authenticates
# by env token, Python ignores the user site entirely, and nothing
# under the shared HOME is consulted again in this step's shell. The
# bats suite already runs hermetically homed — this brings the runner
# to the same shape.
RESTORE_HOME="$(mktemp -d)"
export HOME="$RESTORE_HOME"
export GH_CONFIG_DIR="$RESTORE_HOME/gh"
export PYTHONNOUSERSITE=1
# Replace objects are off from the first git call in this step on (the
# round-8 finding): the model shell can create refs/replace/<SHA>
# entries during the model step, and git resolves every SHA through
# them by default — the reset below must land on the true RESTORE_SHA
# tree, never a model-crafted replacement of it. Exported like the
# config pins above, and for the same reason: the calling step's own
# later git calls must run under it too. The workflow's step env pins
# the same value from process start, covering the staging git show
# that runs before this script is sourced, and the reset itself
# repeats the pin as --no-replace-objects on the command line.
export GIT_NO_REPLACE_OBJECTS=1
# The repository's own .git/config still parses under the source pins
# above, and the model shell can write it — so it is replaced outright,
# BEFORE the first git invocation (a git config call would itself parse
# the model-written file first; this is a pure shell write), with the
# minimal trusted configuration the checkout genuinely needs: the core
# block git init writes for a standard non-bare repository, plus the
# origin remote the post-step fetches need — its URL rebuilt from REPO,
# the workflow-expression owner/name the calling step passes, never
# read back from the model-writable config. Nothing in it is sourced
# from model-writable state; extensions.worktreeConfig is deliberately
# absent (a planted .git/config.worktree never parses), and so are
# url.*.insteadOf, filter.* and every other key a model-written file
# could carry. core.hooksPath is the one deliberate addition (the
# round-9 finding): pinned to /dev/null, it redirects every hook
# lookup away from the model-writable .git/hooks directory — the
# default hooks location no other pin covers — so no model-crafted
# hook script can execute in any git command the step runs.
: "${REPO:?review-restore.sh: REPO required — the trusted owner/name the minimal origin URL is rebuilt from}"
printf '[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = false\n\tlogallrefupdates = true\n\thooksPath = /dev/null\n[remote "origin"]\n\turl = https://github.com/%s\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n' \
  "$REPO" > .git/config
# .git/info/attributes is model-writable and parses as an attribute
# source in every git call — above the worktree's own .gitattributes —
# and no pin or replacement covers it (the round-8 finding): the
# config replacement above deletes any filter definition planted
# there, but a persistent model-spawned child can re-add one between
# this script's git calls, and a binding planted in info/attributes
# would then be live. Deleted outright, alongside the config
# replacement and before the first git invocation, like it: the only
# attribute sources left in any later git call are the trusted
# worktree .gitattributes (the reset below restores it) and the
# pinned-empty core.attributesFile.
rm -f .git/info/attributes
# .git/info/grafts is model-writable too and a graft parenting the base
# onto the head would silently empty the merge-base the facts
# recomputation resolves (the round-11 finding) — deleted alongside the
# attributes, before the first git invocation; the step env also pins
# GIT_GRAFT_FILE=/dev/null so a re-planted file cannot take effect.
rm -f .git/info/grafts
# The worktree .gitattributes is model-writable and parses during the
# reset's checkout (bindings apply before the trusted tree restores it);
# with the config sources dead a binding has no filter definition to
# run, but every .gitattributes is deleted anyway — the root one AND
# the per-directory ones (a model-written dir/.gitattributes parses
# the same way, the round-17 finding) — and the reset restores the
# trusted copies.
find . -name .gitattributes -type f -delete
# The git invocations below carry the full per-invocation pin set: an
# empty core.fsmonitor disables a model-written fsmonitor hook on every
# index-refreshing command here (reset, clean, ls-files), core.pager=cat
# can never spawn a model-written pager, core.worktree=. and
# core.attributesFile= hold the round-6 redirect and filter channels
# closed even in a shell where the replacement above has not run (the
# staging path sources this script exactly there),
# protocol.ext.allow=never refuses the ext:: transport an insteadOf
# rewrite could otherwise reach, and core.hooksPath=/dev/null keeps
# every hook lookup off the model-writable .git/hooks directory even
# where the replacement has not run (the round-9 finding: probed on
# git 2.43/2.54, an undefended reset --hard executes a model-crafted
# .git/hooks/reference-transaction). The reset also carries
# --no-replace-objects: it is the one invocation here that resolves a
# payload SHA, and a model-created refs/replace entry must not steer
# which tree it lands on.
: "${RESTORE_SHA:?review-restore.sh: RESTORE_SHA required — the trusted reset target from the event payload (local HEAD is model-moveable)}"
# The index is model-writable state the reset would otherwise honor:
# git deliberately preserves skip-worktree entries across reset --hard,
# so a model step that marked the post-step scripts skip-worktree and
# wrote its own bytes into them would survive the trusted reset and
# run in this token-bearing step (the round-28 finding) — the same
# model-writes-git-state family as grafts, commondir, and attributes.
# Deleting the index outright kills every carried bit (skip-worktree,
# assume-unchanged, any crafted entry state); the reset below rebuilds
# it from the trusted tree.
rm -f .git/index
git --no-replace-objects -c core.fsmonitor= -c core.pager=cat -c core.worktree=. -c protocol.ext.allow=never -c core.attributesFile= -c core.hooksPath=/dev/null reset --hard "$RESTORE_SHA"
if [ "$NO_CLEAN" -ne 1 ]; then
  git -c core.fsmonitor= -c core.pager=cat -c core.worktree=. -c protocol.ext.allow=never -c core.attributesFile= -c core.hooksPath=/dev/null clean -fdx -- . ':(exclude).stellar'
fi

# A force-added .stellar SYMLINK (e.g. .stellar -> docs) is tracked at
# HEAD, so it survives the reset above and the .stellar-excluding clean
# below — and the findings guard's literal paths then resolve into the
# committed tree it points at, where a pass-shaped document poses as
# model output whenever the model run wrote nothing (the round-12
# finding, re-opening the tracked-pass bypass the guard exists to
# close). .stellar must be a REAL directory: a symlink (or any
# non-directory) is replaced outright before the guard runs.
if [ -L .stellar ]; then
  rm -f .stellar
fi
mkdir -p .stellar

for f in "$@"; do
  if [ -L "$f" ] || { [ -e "$f" ] && [ ! -f "$f" ]; } || git -c core.fsmonitor= -c core.pager=cat -c core.worktree=. -c protocol.ext.allow=never -c core.attributesFile= -c core.hooksPath=/dev/null ls-files --error-unmatch -- "$f" >/dev/null 2>&1; then
    echo "post: rejecting non-model output $f (symlink or tracked at HEAD) — deleting"
    rm -f "$f"
  fi
done

: "${RUNNER_TEMP:?review-restore.sh: RUNNER_TEMP is required (the run step timeout marker lives there)}"
TIMED_OUT=0
if [ -f "$RUNNER_TEMP/review_timed_out" ]; then
  TIMED_OUT=$(cat "$RUNNER_TEMP/review_timed_out")
fi
