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

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The usage text is the leading comment block itself; scripts/lib/usage.sh
# owns the one printer.
usage() {
    usage_text "${BASH_SOURCE[0]}" >&2
    exit 2
}

# Every environment/usage failure reads the same way and exits the same
# code, so the script's prefix, stream and exit contract are stated once
# rather than at each of the guards that enforce it. The conflict and
# compile verdicts do not come through here: they exit 1, a different fact.
# `fail <message> [log]`: the message, then the head of the producer's own
# log when one was captured. The conflict report is not a producer log — it
# is merge-tree's own output, sliced where it is printed.
fail() {
    echo "premerge-check: $1" >&2
    if [ $# -ge 2 ] && [ -f "$2" ]; then
        sed -n '1,10p' "$2" >&2
    fi
    exit 2
}

# Loaded after `fail`, per the loading contract stated once in
# scripts/lib/usage.sh's header (existence check included). `fail` reports a
# missing helper through the environment status (2) — never 1, which this
# gate reserves for "do not merge".
usage_lib="$REPO/scripts/lib/usage.sh"
[ -f "$usage_lib" ] && [ -r "$usage_lib" ] || fail "cannot load scripts/lib/usage.sh (incomplete checkout?)"
# shellcheck source=lib/usage.sh
source "$usage_lib"

# The scratch export is the first thing acquired and the first thing
# released: the trap is installed with it, above every path that can exit.
# Guarded like every other environment fault: an unguarded failure under
# `set -e` would exit 1, the status this gate reserves for "do not merge".
scratch=$(mktemp -d "${TMPDIR:-/tmp}/premerge-check.XXXXXX") \
    || fail "cannot create a scratch directory under ${TMPDIR:-/tmp}"
cleanup() {
    if [ "${PREMERGE_KEEP:-0}" = "1" ]; then
        echo "premerge-check: kept $scratch" >&2
        return
    fi
    rm -rf "$scratch"
}
trap cleanup EXIT

# One anchor for every git call: the script's own repository, never the
# caller's cwd — a run from a subdirectory or from another clone must not
# resolve refs against the wrong repository. First, so its message is the
# one that surfaces when $REPO is not a repository at all.
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repository: $REPO"

# The one "does this name resolve to a commit in $REPO" predicate: the
# anchor, the `^{commit}` peel and the redirection are stated here, so the
# four sites that ask the question cannot drift apart.
is_commit() {
    git -C "$REPO" rev-parse --verify --quiet "$1^{commit}" >/dev/null
}

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
    # Anchored like every git call: gh resolves its repository by walking
    # up from the cwd, so an unanchored lookup run from another clone would
    # read that clone's PR while every git call below runs against $REPO.
    if resolved=$(cd "$REPO" && gh pr view "$1" --json headRefOid,baseRefName \
            --jq '.headRefOid, .baseRefName' 2>"$scratch/gh.err"); then
        # One field per line: the head/base boundary is the newline, not an
        # unwritten no-whitespace assumption parsed back out. A response
        # that does not carry both is an environment error (2), never the
        # exit-1 "do not merge" verdict a bare failing `read` would give.
        { read -r pr_head; read -r pr_base; } <<<"$resolved" || true
        [ -n "$pr_head" ] && [ -n "$pr_base" ] \
            || fail "gh returned an unexpected response for PR $1:" "$scratch/gh.err"
        set -- "$pr_head" "${explicit_base:-$pr_base}"
    elif ! is_commit "$1"; then
        fail "gh could not read PR $1:" "$scratch/gh.err"
    fi
fi

# The defaults are stated once, here: the head is the (possibly rewritten)
# first argument, the base is the second or `main`.
head_rev=$1
base_rev=${2:-main}

is_commit "$head_rev" || fail "$head_rev is not a commit (fetch it first)"

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
            && is_commit "$given"; then
        printf '%s\n' "$given"
        return 0
    fi
    if ! git -C "$REPO" fetch origin "$given" >/dev/null 2>"$scratch/fetch.err"; then
        fail "cannot fetch base branch $given:" "$scratch/fetch.err"
    fi
    ref="refs/remotes/origin/$given"
    if ! is_commit "$ref"; then
        fail "fetching $given did not produce $ref — this clone's refspec does not map it, refusing to merge against an unverified base"
    fi
    printf '%s\n' "$ref"
}
# resolve_base prints the ref or fails. `fail` runs in the substitution's
# subshell, so its exit status is what `|| exit 2` carries into this shell —
# the message still reaches stderr, and the status is still the
# environment one.
base_ref=$(resolve_base "$base_rev") || exit 2


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
git -C "$REPO" merge-tree --write-tree "$base_ref" "$head_rev" \
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
    && git -C "$REPO" archive "$tree" | tar -x -C "$scratch/wt"; } \
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
