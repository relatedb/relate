#!/bin/sh
# Offline, hermetic test harness for scripts/release-bump.sh's CHANGELOG
# rotation — the one place the release-cut paths are pinned end to end:
# the first release (no prior tag, the [Unreleased] ref carrying the
# plain repository URL), the steady-state compare-link path, and the
# populated cut ([Unreleased] carrying entries since the last release —
# they move under the new heading and survive it). The advance's scoped
# mode (--skip-changelog, ADR-0580) is pinned in
# scripts/tests/test_release_advance.sh.
#
# The script operates on the repository it lives in, so each case builds a
# minimal fixture repo under the sandbox (scripts/tests/lib/
# release_bump_harness.sh): the script itself, its helpers, and every file
# the bump touches, with `cargo` and `npm` shadowed by stubs. The fixture
# repo is git-initialized so the failure case can prove the rollback
# restores the changelog.
#
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)

. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox relate-release-bump-test

# The stubs, the fixture factory, and the run helper (no cases here).
. "$ROOT/scripts/tests/lib/release_bump_harness.sh"

# --- Case 1: first release (no prior tag) -------------------------------------

cat >"$sandbox/changelog-first.md" <<'EOF'
# Changelog

## [Unreleased]

### Initial release

First release of Relate.

[Unreleased]: https://github.com/relatedb/relate
EOF

fx1="$sandbox/first"
build_fixture "$fx1" "$sandbox/changelog-first.md"

run_bump "$fx1" 0.1.0
tap_check "first release: bump exits 0" $?

grep -q '^## \[0\.1\.0\] — ' "$fx1/CHANGELOG.md"
tap_check "first release: ## [0.1.0] heading inserted" $?

grep -qF '[Unreleased]: https://github.com/relatedb/relate/compare/v0.1.0...HEAD' \
	"$fx1/CHANGELOG.md"
tap_check "first release: [Unreleased] ref rewritten to compare/v0.1.0...HEAD" $?

grep -qF '[0.1.0]: https://github.com/relatedb/relate/releases/tag/v0.1.0' \
	"$fx1/CHANGELOG.md"
tap_check "first release: [0.1.0] ref points at the release page (no earlier tag)" $?

# --- Case 2: steady state (a prior tag exists) --------------------------------

cat >"$sandbox/changelog-second.md" <<'EOF'
# Changelog

## [Unreleased]

## [0.1.0] — 2026-11-08

### Initial release

First release of Relate.

[Unreleased]: https://github.com/relatedb/relate/compare/v0.1.0...HEAD

[0.1.0]: https://github.com/relatedb/relate/releases/tag/v0.1.0
EOF

fx2="$sandbox/second"
build_fixture "$fx2" "$sandbox/changelog-second.md"

run_bump "$fx2" 0.1.1
tap_check "steady state: bump exits 0" $?

grep -qF '[Unreleased]: https://github.com/relatedb/relate/compare/v0.1.1...HEAD' \
	"$fx2/CHANGELOG.md"
tap_check "steady state: [Unreleased] ref rewritten to compare/v0.1.1...HEAD" $?

grep -qF '[0.1.1]: https://github.com/relatedb/relate/compare/v0.1.0...v0.1.1' \
	"$fx2/CHANGELOG.md"
tap_check "steady state: [0.1.1] ref compares against the previous tag" $?

# ADR-0560: the relate* workspace dependency pins ride the same sweep; the
# path-only entry (relate-test-support, never published) stays untouched.
grep -qF 'relate-model = { path = "relate/relate-model", version = "0.1.1" }' \
	"$fx2/Cargo.toml"
tap_check "steady state: workspace dependency pin swept to the new version" $?

grep -qF 'relate-test-support = { path = "testing/relate-test-support" }' \
	"$fx2/Cargo.toml"
tap_check "steady state: path-only dev-dependency entry untouched" $?

# --- Case 3: populated cut (entries survive the rotation) ----------------------

# The steady-state shape at a real cut: [Unreleased] carries the entries
# since the last release, and the rotation moves them under the new
# version heading — they survive it.

cat >"$sandbox/changelog-populated.md" <<'EOF'
# Changelog

## [Unreleased]

### Added

- Something new since the last release.

## [0.1.0] — 2026-11-08

### Initial release

First release of Relate.

[Unreleased]: https://github.com/relatedb/relate/compare/v0.1.0...HEAD

[0.1.0]: https://github.com/relatedb/relate/releases/tag/v0.1.0
EOF

fx3="$sandbox/populated"
build_fixture "$fx3" "$sandbox/changelog-populated.md"

run_bump "$fx3" 0.1.1
tap_check "populated cut: bump exits 0" $?

grep -q '^## \[0\.1\.1\] — ' "$fx3/CHANGELOG.md"
tap_check "populated cut: ## [0.1.1] heading inserted under [Unreleased]" $?

grep -qF '[Unreleased]: https://github.com/relatedb/relate/compare/v0.1.1...HEAD' \
	"$fx3/CHANGELOG.md"
tap_check "populated cut: [Unreleased] ref rewritten to compare/v0.1.1...HEAD" $?

grep -qF '[0.1.1]: https://github.com/relatedb/relate/compare/v0.1.0...v0.1.1' \
	"$fx3/CHANGELOG.md"
tap_check "populated cut: [0.1.1] ref compares against the previous tag" $?

grep -qF 'Something new since the last release.' "$fx3/CHANGELOG.md"
tap_check "populated cut: the populated entry survives the rotation" $?

# --- Case 4: refusal (no derivable version, no plain repo URL) -----------------

cat >"$sandbox/changelog-bad.md" <<'EOF'
# Changelog

## [Unreleased]

### Initial release

First release of Relate.

[Unreleased]: https://example.com/unknown-shape
EOF

fx4="$sandbox/bad"
build_fixture "$fx4" "$sandbox/changelog-bad.md"

run_bump "$fx4" 0.1.0
[ $? -ne 0 ]
tap_check "refusal: bump exits non-zero" $?

grep -q 'not a plain repository URL' "$fx4/stderr.log"
tap_check "refusal: the diagnostic names the first-release shape" $?

cmp -s "$sandbox/changelog-bad.md" "$fx4/CHANGELOG.md"
tap_check "refusal: the changelog is restored byte-identical" $?

# --- Case 5: refusal (uncommitted edit to a touched path) ---------------------

fx5="$sandbox/dirty"
build_fixture "$fx5" "$sandbox/changelog-first.md"
echo "unrelated local edit" >>"$fx5/README.md"
cp "$fx5/README.md" "$sandbox/readme-dirty.md"

run_bump "$fx5" 0.1.0
[ $? -ne 0 ]
tap_check "dirty tree: bump refuses (exits non-zero)" $?

grep -q 'uncommitted changes in touched paths' "$fx5/stderr.log"
tap_check "dirty tree: the diagnostic names the cause" $?

cmp -s "$sandbox/readme-dirty.md" "$fx5/README.md"
tap_check "dirty tree: the unrelated edit survives byte-identical" $?

cmp -s "$sandbox/changelog-first.md" "$fx5/CHANGELOG.md"
tap_check "dirty tree: nothing else was rewritten" $?

tap_done
