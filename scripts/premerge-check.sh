#!/usr/bin/env bash
# The pre-merge gate: compile the MERGED tree, not the pull request's head.
#
# CI on a pull request builds the head commit. The commit that lands is the
# merge result — head applied on top of whatever `main` is at merge time —
# and the two are not the same tree whenever the branch and `main` have both
# touched the same file. A rename is the sharp case: the branch's hunks
# rename every occurrence it forked with, `main`'s newer copy of the file
# keeps mentions in regions the hunks never reach, the merge is textually
# clean, and the definition the branch deleted is gone. Nothing conflicts;
# nothing compiles.
#
# So: build the merge before making it. `merge-tree` decides the merge in
# object space, the result is materialized in a scratch export, and the
# workspace is type-checked there. A conflict is reported as a conflict; a
# tree that merges but does not compile is the case this exists for.
#
# Usage:
#   scripts/premerge-check.sh <pr-number> [<base>]
#   scripts/premerge-check.sh <rev> [<base>]
#
#   <pr-number>   a GitHub pull request: its head is taken from `gh`, and
#                 its base is `<base>` when given, else the PR's own base
#                 branch.
#   <rev>         any commit-ish to merge.
#   <base>        the base to merge onto: a branch name (default `main`),
#                 fetched from the remote and answered through the ref that
#                 fetch wrote — never a stale local branch; or a full 40- or
#                 64-character commit id, used as it stands (an
#                 after-the-fact check against a pinned base). A shorter
#                 hex spelling is a name like any other: it is fetched.
#
# Env:
#   PREMERGE_TARGET_DIR   cargo target dir (default: a scratch dir removed
#                         with the export). A relative value is resolved
#                         against the CALLER's cwd, not the export's.
#   PREMERGE_KEEP=1       keep the scratch export and print its path, for
#                         inspecting a failure by hand.
#
# Exit: 0 the merged tree type-checks; 1 the merge conflicts or the merged
#       tree does not compile — either way, do not merge; 2 usage or
#       environment error (nothing was concluded about the merge).

set -euo pipefail

# The usage text is the leading comment block itself, printed by content —
# a line-numbered slice would drift the moment the header grows or shrinks.
usage() {
    awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0" >&2
    exit 2
}

# Every environment/usage failure reads the same way and exits the same
# code, so the script's prefix, stream and exit contract are stated once
# rather than at each of the guards that enforce it. The conflict and
# compile verdicts do not come through here: they exit 1, a different fact.
# The head of a captured producer log — the one truncation spelling, shared
# by `fail` and by `resolve_base` (which returns a status rather than
# exiting, and so cannot call `fail`).
log_head() {
    [ -f "$1" ] || return 0
    sed -n "1,${2:-10}p" "$1" >&2
}

# `fail <message> [log]`: the message, then the head of the producer's own
# log when one was captured.
fail() {
    echo "premerge-check: $1" >&2
    [ $# -lt 2 ] || log_head "$2"
    exit 2
}

# The scratch export is the first thing acquired and the first thing
# released: the trap is installed with it, above every path that can exit.
scratch=$(mktemp -d "${TMPDIR:-/tmp}/premerge-check.XXXXXX")
cleanup() {
    if [ "${PREMERGE_KEEP:-0}" = "1" ]; then
        echo "premerge-check: kept $scratch" >&2
        return
    fi
    rm -rf "$scratch"
}
trap cleanup EXIT

# 1 or 2 positionals; a third is a typo, not a silently ignored argument.
[ $# -ge 1 ] && [ $# -le 2 ] || usage

# A bare all-digit argument is ambiguous — a short commit id can be all
# digits too. It is read as a PR number, and only an argument that also
# names a local commit falls back to the rev defaults: a `gh` that is
# missing or failing is an environment error, never a silent fall back to
# treating a PR number as a rev. A successful lookup rewrites the
# positional arguments rather than assigning the names, so an explicitly
# supplied base survives it and each name is assigned exactly once below.
explicit_base=${2:-}
if [[ $1 =~ ^[0-9]+$ ]]; then
    command -v gh >/dev/null || fail "gh is not on PATH — pass the head rev explicitly"
    if resolved=$(gh pr view "$1" --json headRefOid,baseRefName \
            --jq '.headRefOid + " " + .baseRefName' 2>"$scratch/gh.err"); then
        set -- "${resolved%% *}" "${explicit_base:-${resolved#* }}"
    elif ! git rev-parse --verify --quiet "$1^{commit}" >/dev/null; then
        fail "gh could not read PR $1:" "$scratch/gh.err"
    fi
fi

# The defaults are stated once, here: the head is the (possibly rewritten)
# first argument, the base is the second or `main`.
head_rev=$1
base_rev=${2:-main}

git rev-parse --verify --quiet "$head_rev^{commit}" >/dev/null \
    || fail "$head_rev is not a commit (fetch it first)"

# Resolve the base to the ref the merge runs against. A branch name is
# fetched once, by its bare name, and answered through the ref that fetch
# wrote — and the ref is verified, because a fetch succeeding only proves
# the remote had the branch, not that this clone's refspec mapped it there
# (a single-branch or custom-refspec clone maps other names nowhere). A
# failed fetch is reported with git's own error rather than a guess, and it
# is an error, never a fall back to the stale local branch of that name.
# A base spelled in hex is a commit id used as it stands, for
# after-the-fact checks against a pinned base.
resolve_base() {
    local given=$1 ref
    case $given in
        origin/*) given=${given#origin/} ;;
        refs/remotes/origin/*) given=${given#refs/remotes/origin/} ;;
    esac
    # Only a FULL object id names itself. A shorter hex spelling is a name
    # like any other — a branch or tag can be spelled in hex — so it is
    # fetched and verified rather than trusted as a pinned commit.
    if [[ $given =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] \
            && git rev-parse --verify --quiet "$given^{commit}" >/dev/null; then
        printf '%s\n' "$given"
        return 0
    fi
    if ! git fetch origin "$given" >/dev/null 2>"$scratch/fetch.err"; then
        echo "premerge-check: cannot fetch base branch $given:" >&2
        log_head "$scratch/fetch.err"
        return 1
    fi
    ref="refs/remotes/origin/$given"
    if ! git rev-parse --verify --quiet "$ref^{commit}" >/dev/null; then
        echo "premerge-check: fetching $given did not produce $ref — this clone's refspec does not map it, refusing to merge against an unverified base" >&2
        return 1
    fi
    printf '%s\n' "$ref"
}
# resolve_base owns the failure text; this arm only carries the status.
base_ref=$(resolve_base "$base_rev") || exit 2

toplevel=$(git rev-parse --show-toplevel) || fail "not inside a git work tree"

# A relative target dir is resolved against the caller's cwd, not the
# export's — the export is removed on the way out, and a cache pointed
# inside it would warm nothing and vanish.
case ${PREMERGE_TARGET_DIR:-} in
    "") target_dir=$scratch/target ;;
    /*) target_dir=$PREMERGE_TARGET_DIR ;;
    *) target_dir=$PWD/$PREMERGE_TARGET_DIR ;;
esac

# Environment preflight, before anything is concluded about the merge: a
# missing cargo or an unwritable target dir is exit 2, not a verdict.
command -v cargo >/dev/null || fail "cargo is not on PATH"
mkdir -p "$target_dir" 2>"$scratch/mkdir.err" \
    || fail "cannot create target dir $target_dir" "$scratch/mkdir.err"

# The merge is decided in object space first, so a conflict is a conflict
# and never a half-applied tree. merge-tree exits 1 for a conflict and
# something other than 0 or 1 for an error (unsupported option, corrupt
# object): the two are not the same verdict, and collapsing them would
# report a broken environment as a conflict.
echo "premerge-check: merging $head_rev onto $base_rev"
merge_status=0
git -C "$toplevel" merge-tree --write-tree "$base_ref" "$head_rev" \
    >"$scratch/tree" 2>"$scratch/tree.err" || merge_status=$?
case $merge_status in
    0) ;;
    1)
        # merge-tree writes the conflict report to stdout, after the tree
        # OID on line 1; stderr carries only fatal errors (the arm below).
        echo "premerge-check: the merge CONFLICTS — resolve before merging:" >&2
        sed -n '2,40p' "$scratch/tree" >&2
        exit 1
        ;;
    *)
        fail "merge-tree failed (status $merge_status)" "$scratch/tree.err"
        ;;
esac
tree=$(head -n 1 "$scratch/tree")
echo "premerge-check: merged tree $tree"

# Materialize exactly that tree. It is a tree, not a commit — a worktree
# wants a commit-ish, so the tree is exported instead: the bytes checked
# here are the merge's own, not a second merge's.
{ mkdir -p "$scratch/wt" \
    && git -C "$toplevel" archive "$tree" | tar -x -C "$scratch/wt"; } \
    || fail "could not materialize the merged tree"

echo "premerge-check: type-checking the merged tree (target dir $target_dir)"
if ! (cd "$scratch/wt" && CARGO_TARGET_DIR="$target_dir" \
        cargo check --workspace --exclude relate-python); then
    echo >&2
    echo "premerge-check: THE MERGED TREE DOES NOT COMPILE." >&2
    echo "premerge-check: this is the commit CI on the head cannot see — the" >&2
    echo "premerge-check: merge would break $base_rev. Rebase the branch onto" >&2
    echo "premerge-check: $base_rev, fix, and re-run." >&2
    exit 1
fi

echo "premerge-check: the merged tree compiles"
