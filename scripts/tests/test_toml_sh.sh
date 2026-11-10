#!/bin/sh
# TAP suite for the scripts/lib/toml.sh writer contract (the readers are
# exercised everywhere; the writers' contract — match-or-fail, mode
# preserved, no sidecar left behind — is pinned here).
#
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)

. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox relate-toml-sh-test

# shellcheck source=../lib/toml.sh
. "$ROOT/scripts/lib/toml.sh"

manifest="$sandbox/Cargo.toml"
cat >"$manifest" <<'EOF'
[workspace.package]
version = "0.16.1"

[workspace]
members = []
EOF
chmod 644 "$manifest"

lock="$sandbox/Cargo.lock"
cat >"$lock" <<'EOF'
version = 4

[[package]]
name = "relate"
version = "0.16.1"

[[package]]
name = "relate-cli"
version = "0.16.1"
EOF

# --- toml_section_set_version ------------------------------------------------

toml_section_set_version "$manifest" "[workspace.package]" 0.1.0
[ "$(toml_section_version "$manifest" '[workspace.package]')" = "0.1.0" ]
tap_check "section writer: rewrites the version in the named section" $?

[ "$(stat -c '%a' "$manifest" 2>/dev/null || stat -f '%Lp' "$manifest")" = "644" ]
tap_check "section writer: the target's mode survives the replace" $?

# Assert through the helper that owns the sidecar shape, never a literal
# glob: create one in the writers' shape, remove it, expect it gone.
sidecar="$manifest.tmp.test$$"
touch "$sidecar"
toml_remove_sidecars "$manifest"
[ ! -e "$sidecar" ]
tap_check "sidecar cleanup: toml_remove_sidecars removes the writers' shape" $?

tap_run_failure "section writer: no sidecar is left behind" \
	ls "$manifest".tmp.*

tap_run_failure "section writer: a missing section exits non-zero" \
	toml_section_set_version "$manifest" "[nonexistent]" 0.2.0

grep -q 'version = "0.1.0"' "$manifest"
tap_check "section writer: a failed write leaves the target untouched" $?

# --- toml_lock_set_version ----------------------------------------------------

toml_lock_set_version "$lock" relate 0.1.0
[ "$(toml_lock_package_version "$lock" relate)" = "0.1.0" ]
tap_check "lock writer: rewrites the named package's version" $?

[ "$(toml_lock_package_version "$lock" relate-cli)" = "0.16.1" ]
tap_check "lock writer: other packages are untouched" $?

tap_run_failure "lock writer: an absent package exits non-zero" \
	toml_lock_set_version "$lock" no-such-package 0.1.0

grep -q 'name = "relate-cli"' "$lock" && grep -q 'version = "0.16.1"' "$lock"
tap_check "lock writer: a failed write leaves the lockfile untouched" $?

# --- toml_workspace_deps_set_version ------------------------------------------

deps="$sandbox/Cargo-deps.toml"
cat >"$deps" <<'EOF'
[workspace.dependencies]
relate-model = { path = "relate/relate-model", version = "0.16.1" }
relate-test-support = { path = "testing/relate-test-support" }
proptest = "1.11"
EOF

toml_workspace_deps_set_version "$deps" 0.1.0
[ "$(toml_workspace_dep_versions "$deps")" = "relate-model 0.1.0
relate-test-support -" ]
tap_check "workspace-deps writer: rewrites the pin, the reader sees it" $?

grep -qF 'relate-test-support = { path = "testing/relate-test-support" }' "$deps" \
	&& grep -qF 'proptest = "1.11"' "$deps"
tap_check "workspace-deps writer: path-only and external entries are untouched" $?

# The per-entry contract: the writer fails when any entry violates the pin
# policy — a pinless non-exempt entry, a pinned exempt entry, or no
# rewritable pin at all — never a silent pass-through.
cat >"$deps" <<'EOF'
[workspace.dependencies]
relate-test-support = { path = "testing/relate-test-support" }
EOF

tap_run_failure "workspace-deps writer: a pinless section exits non-zero" \
	toml_workspace_deps_set_version "$deps" 0.1.0

grep -qF 'relate-test-support = { path = "testing/relate-test-support" }' "$deps"
tap_check "workspace-deps writer: a failed write leaves the file untouched" $?

cat >"$deps" <<'EOF'
[workspace.dependencies]
relate-model = { path = "relate/relate-model", version = "0.16.1" }
relate-core = { path = "relate/relate-core" }
EOF

tap_run_failure "workspace-deps writer: a pinless non-exempt entry exits non-zero" \
	toml_workspace_deps_set_version "$deps" 0.1.0

grep -qF 'relate-core = { path = "relate/relate-core" }' "$deps" \
	&& grep -qF 'version = "0.16.1"' "$deps"
tap_check "workspace-deps writer: the drift failure leaves every entry untouched" $?

cat >"$deps" <<'EOF'
[workspace.dependencies]
relate-model = { path = "relate/relate-model", version = "0.16.1" }
relate-test-support = { path = "testing/relate-test-support", version = "0.16.1" }
EOF

tap_run_failure "workspace-deps writer: a pinned exempt entry exits non-zero" \
	toml_workspace_deps_set_version "$deps" 0.1.0

grep -qF 'relate-test-support = { path = "testing/relate-test-support", version = "0.16.1" }' "$deps"
tap_check "workspace-deps writer: the pinned exempt entry is left untouched" $?

# A relate* entry whose inline table wraps across lines is grammar this
# vocabulary does not rewrite: both reader and writer fail loudly rather
# than half-reading it.
cat >"$deps" <<'EOF'
[workspace.dependencies]
relate-model = { path = "relate/relate-model",
  version = "0.16.1" }
EOF

tap_run_failure "workspace-deps writer: a wrapped entry exits non-zero" \
	toml_workspace_deps_set_version "$deps" 0.1.0

tap_run_failure "workspace-deps reader: a wrapped entry exits non-zero" \
	toml_workspace_dep_versions "$deps"

tap_done
