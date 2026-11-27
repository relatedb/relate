#!/bin/sh
# The publish-closure pre-push gate (ADR-0665).
#
# The Vortex property-store experiment links a git fork of vortex. That
# dependency must never appear in a published crate's manifest: cargo refuses
# to package a crate with an unversioned dependency, whatever feature-gates it.
# Package the two crates that carry the property-store seams so a leak fails
# here rather than at release time.
#
# Run it before a push that touches a Cargo.toml or a `relate/*` seam; the
# full pre-push gate also runs the build and clippy commands AGENTS.md names.
#
# Before a release cut, the internal path-dep versions are ahead of the
# registry and cargo stops at `failed to select a version for the requirement
# relate-catalog = "^1.2.1"` — a release-order precondition, not a manifest
# leak. The leak this gate catches reads `dependency \`vortex\` does not
# specify a version`.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
for crate in relate-executor relate-storage; do
    echo "check-package: cargo package -p $crate --no-verify" >&2
    cargo package -p "$crate" --no-verify
done
