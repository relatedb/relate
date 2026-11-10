#!/usr/bin/env bash
# release-bump.sh — bump every version surface, rotate CHANGELOG.md, and
# refresh Cargo.lock for a Relate release.
#
# Usage:
#   NEXT_VERSION=0.4.0 scripts/release-bump.sh
#
# NEXT_VERSION is a bare semver (X.Y.Z, no leading "v"). The same string is
# used for the Rust workspace and the Python package (stable releases need no
# PEP 440 translation).
#
# Version surfaces beyond the Rust/Python pair (ADR-0278; every client
# package is lockstep with the release tag):
#   clients/java/pom.xml                               (the project's own
#                                                      <version>, anchored to
#                                                      the artifactId block)
#   clients/javascript/package.json                    ("version" + the five
#                                                       optionalDependencies
#                                                       platform pins)
#   clients/javascript/package-lock.json               (regenerated with
#                                                       npm install
#                                                       --package-lock-only,
#                                                       never sed'd by hand)
#   clients/javascript/platforms/linux-x64/package.json
#   clients/javascript/platforms/linux-arm64/package.json
#   clients/javascript/platforms/darwin-x64/package.json
#   clients/javascript/platforms/darwin-arm64/package.json
#   clients/javascript/platforms/win32-x64/package.json
# The seventh npm manifest, @relate/relate-wasm, is derived:
# surfaces/relate-wasm/npm/package.json.template carries __VERSION__ and
# scripts/build-wasm.sh stamps it from the workspace Cargo.toml (bumped
# above), so it needs no edit here.
#
# Runs in CI on ubuntu-latest (GNU sed); the sedi helper below also works with
# BSD sed so local rehearsals on macOS behave the same.

set -euo pipefail

usage() {
  echo "usage: NEXT_VERSION=<X.Y.Z> $0" >&2
  echo "  NEXT_VERSION: release version without a v prefix, e.g. 0.4.0" >&2
}

next="${NEXT_VERSION:-}"
if [[ -z "$next" || ! "$next" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  usage
  exit 1
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# The pom's project <version> accessor — namespace-agnostic, direct
# children of <project> only; `get` exits 1 when the entry is absent, and
# `set` rewrites only that one line. The step-1 sweep and the step-4
# read-back both call it, so read and write share one parser.
pom_py="$repo_root/scripts/lib/pom.py"

# toml_section_version, toml_section_set_version and
# toml_lock_package_version (the shared TOML vocabulary) back the sweep and
# the consistency self-check.
# shellcheck source=lib/toml.sh
source "$repo_root/scripts/lib/toml.sh"

cargo_toml="Cargo.toml"
pyproject="surfaces/relate-python/pyproject.toml"
pyinit="surfaces/relate-python/python/relate/__init__.py"
lockfile="Cargo.lock"
changelog="CHANGELOG.md"
java_pom="clients/java/pom.xml"
js_main_pkg="clients/javascript/package.json"
js_lockfile="clients/javascript/package-lock.json"
js_platform_pkgs=(
  "clients/javascript/platforms/linux-x64/package.json"
  "clients/javascript/platforms/linux-arm64/package.json"
  "clients/javascript/platforms/darwin-x64/package.json"
  "clients/javascript/platforms/darwin-arm64/package.json"
  "clients/javascript/platforms/win32-x64/package.json"
)
# Prose and examples that quote the Relate version. Membership rule: a
# file belongs here when every version token it carries is the product
# version, and only then — the sweep rewrites each token it finds, so a file
# that also quotes a different version (another tool's release, a dependency
# pin) would be corrupted. What counts as a version token is
# scripts/version_tokens.py's rule, the one implementation behind both the
# step-1 sweep and the step-4 self-check.
docs_version_files=(
  "README.md"
  "docs/limitations.md"
  "docs/reference/rust-library.md"
  "docs/reference/cli-flags.md"
  "docs/api/cli/examples/agents.md"
)
touched=("$cargo_toml" "$pyproject" "$pyinit" "$lockfile" "$changelog" "$java_pom" "$js_main_pkg" "$js_lockfile" "${js_platform_pkgs[@]}" "${docs_version_files[@]}")
# The rollback flag; when the EXIT trap restores is the failure policy's one
# statement (section 0).
ok=0

# sed -i that works with both GNU and BSD sed (-i.bak is accepted by both).
sedi() {
  local file=$1
  shift
  sed -i.bak "$@" "$file"
  rm -f "$file.bak"
}

# npm JSON reads (manifest and lockfile versions, platform pins) go through
# one typed parse; see the module docstring for the subcommands.
npm_json_py="$repo_root/scripts/lib/npm_json.py"
npm_json() {
  python3 "$npm_json_py" "$@"
}

# Restore every touched path, remove the sweep's sidecar files, and exit 1.
# When this runs is the failure policy's one statement (section 0); this
# function only disarms the traps first so its own exit cannot re-trigger.
restore_touched() {
  trap - EXIT INT TERM HUP
  echo "release-bump: $1; restoring touched files" >&2
  local f
  for f in "${touched[@]}"; do
    rm -f "$f.tmp" "$f.bak"
  done
  if git rev-parse --is-inside-work-tree > /dev/null 2>&1; then
    git checkout -- "${touched[@]}"
  else
    echo "release-bump: cannot restore: not inside a git work tree" >&2
  fi
  exit 1
}

# --- 0. Failure policy --------------------------------------------------------
# The rollback idiom: every non-zero exit restores. `ok` stays 0 for the whole
# run and flips to 1 only beside the final success message, so the EXIT trap
# below restores every touched path unless the run reached that point. A
# failing command (set -e exits), an explicit `exit 1`, and a signal — INT,
# TERM, HUP, a CI cancel — all land in the same restore: no per-site failure
# routing exists in the steps below.
#
# Rollback restores to HEAD, so it would destroy uncommitted edits to a
# touched path; refuse up front, before the traps arm (nothing to restore yet).
if git rev-parse --is-inside-work-tree > /dev/null 2>&1 \
  && ! git diff --quiet HEAD -- "${touched[@]}"; then
  echo "release-bump: refusing to run: uncommitted changes in touched paths (rollback restores to HEAD):" >&2
  git diff --name-only HEAD -- "${touched[@]}" >&2
  exit 1
fi
trap '(( ok )) || restore_touched "unexpected failure"' EXIT
trap 'restore_touched "interrupted"' INT TERM HUP

# --- 1. Version surfaces -----------------------------------------------------

toml_section_set_version "$cargo_toml" "[workspace.package]" "$next"
# The relate* entries in [workspace.dependencies] carry a version pin so the
# publishable crates can reach crates.io (ADR pending); the pins ride the
# same lockstep version as every other surface.
toml_workspace_deps_set_version "$cargo_toml" "$next"
toml_section_set_version "$pyproject" "[project]" "$next"
sedi "$pyinit" "s/return \"[0-9][^\"]*\"/return \"$next\"/g"
# Prose and examples that quote the Relate version — the sweep's own
# membership rule sits at the docs_version_files definition above.
python3 scripts/version_tokens.py rewrite --to "$next" "${docs_version_files[@]}"

# npm manifests (ADR-0278). The platform package.json files carry exactly one
# "version" key; the main package.json additionally pins the five platform
# packages in optionalDependencies. The top-level "version" sits at 2-space
# indent while the pins are scoped-name keys at 4-space indent, so the two
# anchored substitutions are unambiguous.
sedi "$js_main_pkg" "s/^  \"version\": \"[^\"]*\"/  \"version\": \"$next\"/"
sedi "$js_main_pkg" -E "s/\"(@relate\/[a-z0-9-]+)\": \"[^\"]*\"/\"\1\": \"$next\"/g"
for f in "${js_platform_pkgs[@]}"; do
  sedi "$f" "s/^  \"version\": \"[^\"]*\"/  \"version\": \"$next\"/"
done

# Maven: the project's own <version> only, through the same structural
# accessor the step-4 read-back calls — dependency and plugin versions are
# never touched.
python3 "$pom_py" set "$java_pom" "$next"

# Regenerate the npm lockfile from the bumped manifest instead of sed'ing it:
# the lockfile's root-entry "version" sits at a different indent than the
# manifest anchor, and hand-editing lockfiles drifts from what npm would
# write. Without this the committed lockfile goes stale after a release and
# npm ci hard-fails. --package-lock-only writes no node_modules; the five
# platform pins are optionalDependencies, so npm tolerates the new version
# not existing on the registry yet (nothing is published at bump time).
npm install --package-lock-only --prefix clients/javascript

# --- 2. Cargo.lock ------------------------------------------------------------

cargo metadata --format-version 1 > /dev/null

# --- 3. CHANGELOG rotation ----------------------------------------------------

# The whole rotation policy — the [Unreleased] ref's shape (compare link =
# steady state, plain repository URL = first release), the heading grammar,
# the idempotence decision, the link-ref rewrites — lives in
# scripts/changelog_rotate.py, the one implementation, unit-tested directly.
# Already-rotated is the module's own skip notice on stdout, exit 0.
python3 scripts/changelog_rotate.py "$changelog" \
  --version "$next" --date "$(date +%F)"

# --- 4. Consistency self-check (after editing) --------------------------------

# The checks below collect their findings here so every inconsistency is
# named before one non-zero exit (the EXIT trap does the restoring).
fail=0

# expect_eq <label> <expected> <actual> — the one spelling of the step-4
# check block: compare, name the mismatch, set fail.
expect_eq() {
  [[ "$3" == "$2" ]] || {
    echo "release-bump: $1 is '$3', expected '$2'" >&2
    fail=1
  }
}

# read_back_eq <label> <missing-message> <reader...> — for readers whose
# non-zero status means the entry is absent or malformed, distinct from a
# wrong value; both arms set fail.
read_back_eq() {
  local label="$1" missing="$2" value
  shift 2
  if value="$("$@")"; then
    expect_eq "$label" "$next" "$value"
  else
    echo "release-bump: $missing" >&2
    fail=1
  fi
}

check_cargo="$(toml_section_version "$cargo_toml" "[workspace.package]")"
check_pyproject="$(toml_section_version "$pyproject" "[project]")"

expect_eq "Cargo.toml [workspace.package] version" "$next" "$check_cargo"
expect_eq "pyproject.toml [project] version" "$next" "$check_pyproject"
# Workspace dependency pins: every relate* entry must read back at $next —
# a stale pin would publish a crates.io dependency on a version that does
# not exist.
stale_pins="$(toml_workspace_dep_versions "$cargo_toml" | awk -v v="$next" '$2 != v { print $1 }')"
[[ -z "$stale_pins" ]] || {
  echo "release-bump: Cargo.toml [workspace.dependencies] pins not at $next:" >&2
  echo "$stale_pins" >&2
  fail=1
}
# The fallback-literal grammar has one spelling — `return "<digit...>"` —
# shared by the step-1 sweep and this stale check, so they cannot disagree
# about what a fallback literal is. The invariant is "no stale fallback
# literal survives the sweep" — the count of literals is __init__.py's own
# fact, never mirrored here.
if grep -E 'return "[0-9]' "$pyinit" | grep -v "return \"$next\"" > /dev/null; then
  echo "release-bump: __init__.py still has a stale fallback version literal" >&2
  fail=1
fi
grep -q "return \"$next\"" "$pyinit" || {
  echo "release-bump: __init__.py carries no fallback literal return \"$next\"" >&2
  fail=1
}
# toml_lock_package_version exits non-zero when the relate package is absent
# from the lockfile; read_back_eq keeps absent distinct from wrong.
read_back_eq "Cargo.lock relate package version" \
  "Cargo.lock has no relate package entry" \
  toml_lock_package_version "$lockfile" relate

# Maven: read back through the same accessor; a non-zero status means the
# entry is missing or the pom is malformed — distinct from a wrong value.
read_back_eq "$java_pom project version" \
  "$java_pom has no readable project <version> entry" \
  python3 "$pom_py" get "$java_pom"

# npm manifests: the main package's "version" and its five platform pins, and
# each platform package's "version". Both pin checks (here and in the
# lockfile below) expect the platform-package list's length — the same count
# the sweep above iterates, never a second copy of the number.
js_pin_count="${#js_platform_pkgs[@]}"
check_js_main="$(npm_json version "$js_main_pkg")"
expect_eq "$js_main_pkg version" "$next" "$check_js_main"
check_js_pins="$(npm_json pins "$js_main_pkg" "$next")"
expect_eq "$js_main_pkg optionalDependencies pins at $next" "$js_pin_count" "$check_js_pins"
for f in "${js_platform_pkgs[@]}"; do
  check_platform="$(npm_json version "$f")"
  expect_eq "$f version" "$next" "$check_platform"
done

# npm lockfile: the top-level "version", the packages[""] root entry's
# "version", and the five platform pins.
check_js_lock_root="$(npm_json version "$js_lockfile")"
expect_eq "$js_lockfile top-level version" "$next" "$check_js_lock_root"
check_js_lock_pkg="$(npm_json root-version "$js_lockfile")"
expect_eq "$js_lockfile root-entry version" "$next" "$check_js_lock_pkg"
check_js_lock_pins="$(npm_json lock-pins "$js_lockfile" "$next")"
expect_eq "$js_lockfile optionalDependencies pins at $next" "$js_pin_count" "$check_js_lock_pins"

# Docs sweep: once the rewrite is done every version token in a listed file is
# the product version. The check runs the same tokenizer the sweep used, so the
# two cannot disagree about what a token is; a run the sweep leaves alone
# (README.md's `0.0.0.0` quads) is not a version token and is not stale.
python3 scripts/version_tokens.py check --expect "$next" "${docs_version_files[@]}" || fail=1

if (( fail )); then
  echo "release-bump: consistency check failed" >&2
  exit 1
fi

ok=1
echo "release-bump: all version surfaces set to $next"
