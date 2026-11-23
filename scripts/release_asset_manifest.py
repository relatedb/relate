#!/usr/bin/env python3
"""Release asset manifest: the JSON index of every asset attached to a
release — asset name to sha256 and size — plus the gate that fails the
release when any staged asset would ship without a manifest entry.

Usage:
  release_asset_manifest.py generate <release-dir> [--output <file>]
  release_asset_manifest.py check <release-dir> <manifest>
  release_asset_manifest.py anchor <release-dir> <manifest>

`generate` walks `<release-dir>` (the staged directory whose files the
release attaches) and writes the index; the default output is
`<release-dir>/assets.json`, so the manifest rides the release's assets
the way releases.json does. The manifest never carries an entry for
itself — a file cannot contain its own digest — so both `generate` and
`check` skip a staged file named like the manifest.

`check` exits non-zero, naming every offender on stderr, when an asset in
`<release-dir>` has no manifest entry, when an entry's sha256 or size does
not match the staged file, or when the manifest names an asset that is not
staged. It runs before SHA256SUMS exists: the checksum set covers the
manifest, so the manifest cannot cover the checksum set.

`anchor` is the post-signing gate: the signed `SHA256SUMS` must carry a
line for the manifest, and its digest must be the manifest's own sha256 —
a line that merely names the file anchors nothing. Exit 0 means the signed
checksum set covers these exact manifest bytes.

Run: python3 scripts/release_asset_manifest.py generate release
      python3 scripts/release_asset_manifest.py check release release/assets.json
      python3 scripts/release_asset_manifest.py anchor release release/assets.json
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path

# The one conventional manifest filename; the generate default and the
# release workflow's gate step agree on it through this constant.
DEFAULT_MANIFEST_NAME = "assets.json"

# The signed checksum set: the anchor that binds the manifest to the release.
CHECKSUMS_NAME = "SHA256SUMS"

# A checksum line: "<64-hex>  <name>" (shasum -a 256, two spaces), with an
# optional binary-mode asterisk; the ADR-0457 version header (a `#` line) is
# not matched.
_CHECKSUM_LINE = re.compile(r"^([0-9a-fA-F]{64})[ \t]+\*?(\S+)\s*$", re.MULTILINE)


@dataclass(frozen=True)
class Entry:
    """One asset's manifest entry — the shape every verb builds and reads."""

    sha256: str
    size: int

    def to_json(self) -> dict[str, object]:
        return {"sha256": self.sha256, "size": self.size}

    @classmethod
    def from_json(cls, raw: object) -> "Entry | None":
        """The entry a manifest value declares, or None when the value is not
        an entry of the right shape (a present key of the wrong type is a
        malformed entry, not an absent one). No Entry ever holds a
        wrong-typed field: the sha256 is a string and the size an integer
        that is not a bool."""
        if not isinstance(raw, dict):
            return None
        sha256 = raw.get("sha256")
        size = raw.get("size")
        if not isinstance(sha256, str):
            return None
        if not isinstance(size, int) or isinstance(size, bool):
            return None
        return cls(sha256, size)


def _entry(path: Path) -> Entry:
    """The manifest entry for one staged asset: sha256 hex and size bytes."""
    with path.open("rb") as f:
        digest = hashlib.file_digest(f, "sha256").hexdigest()
    return Entry(digest, path.stat().st_size)


def _required_file(path: Path, label: str) -> Path | None:
    """A required input file, or None with the refusal on stderr when it is
    missing — the one required-input spelling `check` and `anchor` call for
    the manifest and the checksum set."""
    if not path.is_file():
        print(f"{label} '{path}' does not exist", file=sys.stderr)
        return None
    return path


def _staged_assets(release_dir: Path, manifest_name: str) -> list[Path] | None:
    """The staged assets: every file in the directory except the manifest.
    None, with the refusal on stderr, when the directory is missing or holds
    no asset — the one staged-directory policy `generate` and `check` share."""
    if not release_dir.is_dir():
        print(f"release directory '{release_dir}' does not exist", file=sys.stderr)
        return None
    assets = sorted(
        p for p in release_dir.iterdir() if p.is_file() and p.name != manifest_name
    )
    if not assets:
        print(f"no assets found in '{release_dir}'", file=sys.stderr)
        return None
    return assets


def _checksums(path: Path) -> dict[str, str]:
    """The digests a SHA256SUMS file lists, keyed by asset name (the first
    two fields of each `<hex>  <name>` line); the version header is skipped."""
    return {name: digest for digest, name in _CHECKSUM_LINE.findall(path.read_text())}


def cmd_generate(args: argparse.Namespace) -> int:
    output = args.output or args.release_dir / DEFAULT_MANIFEST_NAME
    assets = _staged_assets(args.release_dir, output.name)
    if assets is None:
        return 1
    manifest = {p.name: _entry(p).to_json() for p in assets}
    output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    print(f"wrote {output} with {len(manifest)} entries")
    return 0


def cmd_check(args: argparse.Namespace) -> int:
    manifest_path = _required_file(args.manifest, "manifest")
    if manifest_path is None:
        return 1
    staged = _staged_assets(args.release_dir, manifest_path.name)
    if staged is None:
        return 1
    try:
        manifest = json.loads(manifest_path.read_text())
    except json.JSONDecodeError as err:
        print(f"manifest '{manifest_path}' is not valid JSON: {err}", file=sys.stderr)
        return 1
    if not isinstance(manifest, dict):
        print(f"manifest '{manifest_path}' is not a JSON object", file=sys.stderr)
        return 1

    assets = {p.name: p for p in staged}
    problems: list[str] = []
    for name, path in assets.items():
        # Membership and type are distinct refusals: an absent key is a
        # missing entry, a present one of the wrong type (JSON null included)
        # is a malformed entry.
        if name not in manifest:
            problems.append(f"{name}: no manifest entry")
            continue
        entry = Entry.from_json(manifest[name])
        if entry is None:
            problems.append(f"{name}: manifest entry is malformed")
            continue
        if entry != _entry(path):
            problems.append(f"{name}: manifest sha256/size does not match the staged file")
    for name in manifest:
        if name not in assets:
            problems.append(f"{name}: manifest entry for an asset that is not staged")

    if problems:
        for problem in problems:
            print(problem, file=sys.stderr)
        return 1
    print(f"manifest covers all {len(assets)} assets in '{args.release_dir}'")
    return 0


def cmd_anchor(args: argparse.Namespace) -> int:
    """The post-signing gate: the signed checksum set must carry the
    manifest's line, and its digest must be the manifest's own sha256 — a
    line that merely names the file anchors nothing."""
    manifest_path = _required_file(args.manifest, "manifest")
    if manifest_path is None:
        return 1
    sums = _required_file(args.release_dir / CHECKSUMS_NAME, "checksum set")
    if sums is None:
        return 1
    listed = _checksums(sums).get(manifest_path.name)
    if listed is None:
        print(
            f"{manifest_path.name}: {CHECKSUMS_NAME} carries no line for the manifest",
            file=sys.stderr,
        )
        return 1
    actual = _entry(manifest_path).sha256
    if listed.lower() != actual:
        print(
            f"{manifest_path.name}: {CHECKSUMS_NAME} digest {listed} does not match "
            f"the manifest's sha256 {actual}",
            file=sys.stderr,
        )
        return 1
    print(f"{manifest_path.name}: anchored by {CHECKSUMS_NAME}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Generate and gate the release asset manifest (assets.json)."
    )
    sub = parser.add_subparsers(dest="command", required=True)

    gen = sub.add_parser(
        "generate", help="write the manifest for a staged release directory"
    )
    gen.add_argument("release_dir", type=Path, help="directory of staged release assets")
    gen.add_argument(
        "--output",
        type=Path,
        default=None,
        help=f"manifest path (default: <release-dir>/{DEFAULT_MANIFEST_NAME})",
    )
    gen.set_defaults(func=cmd_generate)

    chk = sub.add_parser(
        "check", help="fail unless the manifest covers every staged asset"
    )
    chk.add_argument("release_dir", type=Path, help="directory of staged release assets")
    chk.add_argument("manifest", type=Path, help="the manifest to check")
    chk.set_defaults(func=cmd_check)

    anc = sub.add_parser(
        "anchor",
        help=f"fail unless {CHECKSUMS_NAME} carries the manifest's digest",
    )
    anc.add_argument("release_dir", type=Path, help="directory of staged release assets")
    anc.add_argument("manifest", type=Path, help="the manifest to anchor")
    anc.set_defaults(func=cmd_anchor)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
