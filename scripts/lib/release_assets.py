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

import json
import re
from pathlib import Path

# The repository every channel's installer URL points into, and the one
# release download route (ADR-0585) its assets ride.
REPO_URL = "https://github.com/relatedb/relate"

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


def release_download_url(tag: str, asset: str) -> str:
    """The release download URL for `asset` under `tag` (ADR-0585). The tag
    may be a template token (`v$version`) a channel's autoupdate fills in."""
    return f"{REPO_URL}/releases/download/{tag}/{asset}"


def load_json_object(path: Path, what: str, error: type[BaseException]) -> dict[str, object]:
    """The JSON object at `path`, or `error` naming what is wrong with it.

    The one load-parse-require-object policy every channel generator reads
    its release asset manifest (and Scoop its bucket manifest) through."""
    if not path.is_file():
        raise error(f"{what} '{path}' does not exist")
    try:
        loaded = json.loads(path.read_text())
    except OSError as err:
        raise error(f"cannot read {what} '{path}': {err}") from err
    except json.JSONDecodeError as err:
        raise error(f"{what} '{path}' is not valid JSON: {err}") from err
    if not isinstance(loaded, dict):
        raise error(f"{what} '{path}' is not a JSON object")
    return loaded
