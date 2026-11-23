#!/usr/bin/env python3
"""The one release tag-to-version policy shared by every channel generator.

A release is cut under a `vX.Y.Z` tag, while the package metadata each
channel publishes carries the bare `X.Y.Z` version (Homebrew scans the
version from the download URL's tag; the Scoop manifest, winget manifests
and RPM spec record the bare version). `scripts/homebrew_formula.py`,
`scripts/install_smoke.py`, `scripts/scoop_manifest.py` and
`scripts/build_rpm_repo.py` each carried their own copy of that conversion
and of the `vX.Y.Z`/`X.Y.Z` shape check, and the copies had drifted. One
helper, one policy: a `vX.Y.Z` tag names an `X.Y.Z` version and back, and
anything else — an extension-channel tag, a prerelease, a bare number — is
refused.

The caller names the exception its own command reports with, so each script
keeps its user-facing error type and message (`FormulaError`,
`argparse.ArgumentTypeError`, `ValueError`).
"""

from __future__ import annotations

import re

# A stable core release tag: extension-channel and prerelease tags never
# match, so no channel ever publishes one.
TAG = re.compile(r"v(\d+\.\d+\.\d+)")
VERSION = re.compile(r"\d+\.\d+\.\d+")


def release_version(tag: str, error: type[Exception] = ValueError) -> str:
    """The bare version a release tag carries: `vX.Y.Z` -> `X.Y.Z`."""
    match = TAG.fullmatch(tag)
    if match is None:
        raise error(f"release tag '{tag}' is not vX.Y.Z")
    return match.group(1)


def release_tag(version: str, error: type[Exception] = ValueError) -> str:
    """The release tag for a bare version: `X.Y.Z` -> `vX.Y.Z`."""
    if VERSION.fullmatch(version) is None:
        raise error(f"release version '{version}' is not X.Y.Z")
    return f"v{version}"
