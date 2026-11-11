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
# object space, the result is materialized in a scratch worktree, and the
# workspace is type-checked there. A conflict is reported as a conflict; a
# tree that merges but does not compile is the case this exists for.
#
# Usage:
#   scripts/premerge-check.sh <pr-number>
#   scripts/premerge-check.sh <rev> [<base-rev>]
#
#   <pr-number>   a GitHub pull request: its head is taken from `gh`, its
#                 base from the PR itself.
#   <rev>         any rev to merge; the base defaults to `origin/main`.
#   <base-rev>    the base to merge onto, instead of `origin/main`.
#
# Env:
#   PREMERGE_TARGET_DIR   cargo target dir (default: a scratch dir that is
#                         removed with the worktree). Point it at a warm
#                         shared dir to make repeat runs cheap.
#   PREMERGE_KEEP=1       keep the scratch worktree and print its path, for
#                         inspecting a failure by hand.
#
# Exit: 0 the merged tree type-checks; 1 it does not (or the merge conflicts);
#       2 usage or environment error.

set -euo pipefail

usage() {
    sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

[ $# -ge 1 ] || usage

# Resolve the two revs before touching the tree: a PR number is looked up,
# anything else is taken as a rev. The base of a PR is the PR's own base
# branch, so a stacked pull request is checked against what it will actually
# merge into rather than against main.
#
# A bare all-digit argument is ambiguous — a short commit SHA can be all
# digits too — so it is tried as a PR first and falls back to a rev.
pr_lookup() {
    command -v gh >/dev/null || return 1
    gh pr view "$1" --json headRefOid,baseRefName --jq '.headRefOid + " " + .baseRefName' 2>/dev/null
}
case $1 in
    *[!0-9]*) head_rev=$1; base_rev=${2:-origin/main} ;;
    *)
        if resolved=$(pr_lookup "$1"); then
            head_rev=${resolved%% *}
            base_rev=${resolved#* }
        else
            head_rev=$1
            base_rev=${2:-origin/main}
        fi
        ;;
esac

git rev-parse --verify --quiet "$head_rev^{commit}" >/dev/null \
    || { echo "premerge-check: $head_rev is not a commit (fetch it first)" >&2; exit 2; }

# A PR base is a branch name, which may be ahead of every local ref; a rev
# argument is a rev. Try the spelling as given, then the remote-tracking
# one, fetching as needed — never fall back to a stale local branch.
resolve_base() {
    local rev=$1
    if git rev-parse --verify --quiet "$rev^{commit}" >/dev/null; then
        printf '%s\n' "$rev"
        return 0
    fi
    git fetch origin "$rev" >/dev/null 2>&1 || true
    if git rev-parse --verify --quiet "origin/$rev^{commit}" >/dev/null; then
        printf '%s\n' "origin/$rev"
        return 0
    fi
    return 1
}
base_rev=$(resolve_base "$base_rev") \
    || { echo "premerge-check: base $base_rev is not a commit" >&2; exit 2; }

toplevel=$(git rev-parse --show-toplevel) || exit 2
scratch=$(mktemp -d "${TMPDIR:-/tmp}/premerge-check.XXXXXX")
target_dir=${PREMERGE_TARGET_DIR:-$scratch/target}

cleanup() {
    if [ "${PREMERGE_KEEP:-0}" = "1" ]; then
        echo "premerge-check: kept $scratch" >&2
        return
    fi
    rm -rf "$scratch"
}
trap cleanup EXIT

# The merge is decided in object space first, so a conflict is a conflict
# and never a half-applied tree.
echo "premerge-check: merging $head_rev onto $base_rev"
if ! git -C "$toplevel" merge-tree --write-tree "$base_rev" "$head_rev" >"$scratch/tree" 2>"$scratch/tree.err"; then
    echo "premerge-check: the merge CONFLICTS — resolve before merging:" >&2
    sed -n '1,40p' "$scratch/tree.err" >&2
    exit 1
fi
tree=$(head -n 1 "$scratch/tree")
echo "premerge-check: merged tree $tree"

# Materialize exactly that tree. It is a tree, not a commit — a worktree
# wants a commit-ish, so the tree is exported instead: the bytes checked
# here are the merge's own, not a second merge's.
mkdir -p "$scratch/wt"
git -C "$toplevel" archive "$tree" | tar -x -C "$scratch/wt"

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
