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

tap_done
