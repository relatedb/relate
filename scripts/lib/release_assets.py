#!/usr/bin/env python3
"""The one read of a release asset manifest's sha256 for one asset.

Every package channel pins the hash its installer verifies: the Scoop
manifest (`scripts/scoop_manifest.py`), the winget manifests
(`scripts/winget_manifests.py`) and the Homebrew formula
(`scripts/homebrew_formula.py`) each took their asset's entry from
assets.json with their own copy of the lookup, and the copies had already
drifted (one skipped hex validation, one skipped the object check). One
helper, one policy: a missing entry, a malformed entry, or a sha256 that
is not 64 lowercase hex digits fails — the manifest carries the published
hash, never a fabricated or re-derived one.

The caller names the exception its own command reports with, so each
script keeps its user-facing error type and message.
"""

from __future__ import annotations

import re

SHA256 = re.compile(r"[0-9a-f]{64}")


def asset_sha256(manifest: object, asset: str, error: type[Exception]) -> str:
    """The release asset manifest's sha256 for `asset`, raised as `error`."""
    if not isinstance(manifest, dict):
        raise error("the release asset manifest is not a JSON object")
    entry = manifest.get(asset)
    if not isinstance(entry, dict):
        raise error(f"the release asset manifest has no entry for '{asset}'")
    sha256 = entry.get("sha256")
    if not isinstance(sha256, str) or SHA256.fullmatch(sha256) is None:
        raise error(
            f"the release asset manifest sha256 for '{asset}' is not 64 hex digits"
        )
    return sha256
