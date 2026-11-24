#!/bin/sh
# The pin grammar's own suite (REL8-2081): scripts/lib/baseline-pin.sh is the one
# home of "is this a pin" and "which part is this", shared by the drift check,
# its test and the pins guard — so its contract is asserted here, not at the
# tail of a suite named for a consumer.
#
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)

. "$ROOT/scripts/tests/lib/tap.sh"
. "$ROOT/scripts/lib/baseline-pin.sh"

digest="sha256:$(printf 'a%.0s' $(seq 64))"
pinned="debian:bookworm-20261123-slim@${digest}"

tap_run_success "a pinned reference is a pin" pin_valid "$pinned"
tap_run_failure "a floating tag is not a pin" pin_valid "debian:bookworm-slim"
tap_run_failure "an empty value is not a pin" pin_valid ""
tap_run_failure "a digest without a tag is not a pin" pin_valid "debian@${digest}"
tap_run_failure "a truncated digest is not a pin" pin_valid "debian:bookworm-slim@sha256:abc"
tap_run_failure "a registry-qualified repo is not an official-library pin" \
	pin_valid "ghcr.io/debian:bookworm-slim@${digest}"
tap_run_failure "a multi-line value is not one pin" pin_valid "junk
${pinned}"

assert_same "name is repo:tag" "$(pin_part "$pinned" name)" "debian:bookworm-20261123-slim"
assert_same "repo is the repository" "$(pin_part "$pinned" repo)" "debian"
assert_same "tag is the tag" "$(pin_part "$pinned" tag)" "bookworm-20261123-slim"
assert_same "digest is the digest" "$(pin_part "$pinned" digest)" "$digest"
tap_run_failure "an unknown part is refused" pin_part "$pinned" nonsense
tap_run_failure "a non-pin has no parts" pin_part "debian:bookworm-slim" repo

tap_done
