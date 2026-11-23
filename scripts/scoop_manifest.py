#!/usr/bin/env python3
"""Scoop bucket manifest for the Windows CLI binary: render it from a
release's asset manifest, and check a manifest against that same manifest.

Usage:
  scoop_manifest.py generate <assets.json> --tag <vX.Y.Z> [--output <file>]
  scoop_manifest.py check <manifest.json> <assets.json> --tag <vX.Y.Z>

`generate` writes the manifest the `relatedb/scoop-bucket` repository
publishes (bucket/relate.json): the version from the release tag, the
installer URL for that tag, and the sha256 the release asset manifest
records for `relate-windows-amd64.exe`. The checkver block reads the
CLI's release-discovery document (`releases.json`, ADR-0546) and takes
its stable `latest.tag` — not GitHub's `releases/latest`, which an
extension-channel release wins; autoupdate re-derives the URL from
`$version` and the hash from `$baseurl/assets.json`, so bumping the tag
keeps URL and hash in step without editing the installer block.

`check` verifies a manifest against the release asset manifest: version,
installer URL, hash against the asset's entry, the `bin` alias that makes
the shim `relate`, and the checkver/autoupdate contract above. Exit 0
means all match; exit 1 names every offender on stderr.

Run: python3 scripts/scoop_manifest.py generate release/assets.json \
        --tag v1.2.0 --output bucket/relate.json
     python3 scripts/scoop_manifest.py check bucket/relate.json release/assets.json --tag v1.2.0
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))

from release_assets import asset_sha256
from release_version import release_version

REPO_URL = "https://github.com/relatedb/relate"

# checkver reads the CLI's own release-discovery document (ADR-0546), not
# GitHub's `releases/latest`: extension-channel releases
# (`relate-extensions-<name>-X.Y.Z`) are ordinary non-prerelease releases,
# so `latest` returns one after any extension cut. releases.json's
# `latest.tag` is the newest stable core release.
CHECKVER_URL = "https://relatedb.ai/releases.json"
CHECKVER_JSONPATH = "$.latest.tag"
CHECKVER_REGEX = r"v(\d+\.\d+\.\d+)"

# The Windows CLI asset and the shim name Scoop exposes it under: the
# asset carries the platform in its filename, users run `relate`.
WINDOWS_ASSET = "relate-windows-amd64.exe"
SHIM = "relate"

# The release-asset-manifest source autoupdate reads its hash from, at the
# bumped tag's download directory; the concrete hash is the same entry.
AUTOUPDATE_HASH_URL = "$baseurl/assets.json"
AUTOUPDATE_JSONPATH = f"$['{WINDOWS_ASSET}'].sha256"

# The tag autoupdate substitutes: release tags carry the `v` the Scoop
# version drops.
VERSION_TEMPLATE = "v$version"


def asset_url(tag: str) -> str:
    """The release asset URL for the tag (`$version` is a valid tag here:
    autoupdate substitutes it into the URL it renders)."""
    return f"{REPO_URL}/releases/download/{tag}/{WINDOWS_ASSET}"


def stored_hash(hash_value: str) -> str:
    """The bare sha256 a manifest hash carries: Scoop writes `sha256:` in a
    hand-authored manifest and bare hex after autoupdate."""
    return hash_value.strip().lower().removeprefix("sha256:")


def build_manifest(tag: str, sha256: str) -> dict[str, object]:
    """The bucket manifest for one release, its hash from the release's
    asset manifest entry for the Windows binary."""
    marker = json.dumps(
        {"manager": "scoop", "upgrade": "scoop update relate"}, separators=(",", ":")
    )
    return {
        "version": release_version(tag),
        "description": "Relate: embedded openCypher graph database with a single-binary CLI",
        "homepage": "https://relatedb.ai",
        "license": "Apache-2.0",
        "architecture": {
            "64bit": {
                "url": asset_url(tag),
                "hash": f"sha256:{sha256}",
            }
        },
        "bin": [[WINDOWS_ASSET, SHIM]],
        # Package-owned installs record themselves beside the real binary
        # (ADR-0659); `$dir` is the extracted version directory, the file
        # `relate upgrade` resolves through the `current` junction.
        "post_install": (
            "[IO.File]::WriteAllText((Join-Path $dir '.relate-package-managed'), "
            f"'{marker}')"
        ),
        "checkver": {
            "url": CHECKVER_URL,
            "jsonpath": CHECKVER_JSONPATH,
            "regex": CHECKVER_REGEX,
        },
        "autoupdate": {
            "architecture": {
                "64bit": {
                    "url": asset_url(VERSION_TEMPLATE),
                    "hash": {"url": AUTOUPDATE_HASH_URL, "jsonpath": AUTOUPDATE_JSONPATH},
                }
            }
        },
    }


def check(manifest: dict[str, object], assets: dict[str, object], tag: str) -> list[str]:
    """Every way the manifest disagrees with the release and its asset
    manifest, or its own checkver/autoupdate contract."""
    errors: list[str] = []
    expected_version = release_version(tag)
    wanted_url = asset_url(tag)

    try:
        expected_hash = stored_hash(asset_sha256(assets, WINDOWS_ASSET, KeyError))
    except KeyError as err:
        return [str(err.args[0])]

    if manifest.get("version") != expected_version:
        errors.append(f"version: {manifest.get('version')!r} != {expected_version!r}")

    architecture = manifest.get("architecture")
    arch = architecture.get("64bit") if isinstance(architecture, dict) else None
    if not isinstance(arch, dict):
        return errors + ["architecture.64bit: missing"]
    if arch.get("url") != wanted_url:
        errors.append(f"url: {arch.get('url')!r} != {wanted_url!r}")
    if not isinstance(arch.get("hash"), str) or stored_hash(arch["hash"]) != expected_hash:
        errors.append(f"hash: {arch.get('hash')!r} != sha256:{expected_hash}")

    if manifest.get("bin") != [[WINDOWS_ASSET, SHIM]]:
        errors.append(f"bin: {manifest.get('bin')!r} != [[{WINDOWS_ASSET!r}, {SHIM!r}]]")

    checkver = manifest.get("checkver")
    expected_checkver = {
        "url": CHECKVER_URL,
        "jsonpath": CHECKVER_JSONPATH,
        "regex": CHECKVER_REGEX,
    }
    if checkver != expected_checkver:
        errors.append(f"checkver: {checkver!r} != {expected_checkver!r}")

    autoupdate = manifest.get("autoupdate")
    update_arch = None
    if isinstance(autoupdate, dict) and isinstance(autoupdate.get("architecture"), dict):
        update_arch = autoupdate["architecture"].get("64bit")
    if not isinstance(update_arch, dict):
        return errors + ["autoupdate.architecture.64bit: missing"]
    if update_arch.get("url") != asset_url(VERSION_TEMPLATE):
        errors.append(
            f"autoupdate url: {update_arch.get('url')!r} != {asset_url(VERSION_TEMPLATE)!r}"
        )
    update_hash = update_arch.get("hash")
    if not isinstance(update_hash, dict) or update_hash.get("url") != AUTOUPDATE_HASH_URL:
        errors.append(f"autoupdate hash url: {update_hash!r} != {AUTOUPDATE_HASH_URL!r}")
    elif update_hash.get("jsonpath") != AUTOUPDATE_JSONPATH:
        errors.append(
            f"autoupdate hash jsonpath: {update_hash.get('jsonpath')!r} != {AUTOUPDATE_JSONPATH!r}"
        )
    return errors


def _load(path: Path, what: str) -> dict[str, object]:
    try:
        loaded = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as err:
        raise SystemExit(f"{what} '{path}' is not readable JSON: {err}")
    if not isinstance(loaded, dict):
        raise SystemExit(f"{what} '{path}' is not a JSON object")
    return loaded


def _tag(value: str) -> str:
    release_version(value, argparse.ArgumentTypeError)
    return value


def cmd_generate(assets_path: Path, tag: str, output: Path) -> int:
    assets = _load(assets_path, "release asset manifest")
    try:
        sha256 = asset_sha256(assets, WINDOWS_ASSET, KeyError)
    except KeyError as err:
        print(err.args[0], file=sys.stderr)
        return 1
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(build_manifest(tag, sha256), indent=4) + "\n")
    print(f"wrote {output} for {tag}")
    return 0


def cmd_check(manifest_path: Path, assets_path: Path, tag: str) -> int:
    manifest = _load(manifest_path, "manifest")
    assets = _load(assets_path, "release asset manifest")
    errors = check(manifest, assets, tag)
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print(f"{manifest_path}: matches {assets_path} for {tag}")
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    generate = sub.add_parser("generate", help="render the bucket manifest")
    generate.add_argument("assets", type=Path, help="the release's assets.json")
    generate.add_argument("--tag", type=_tag, required=True)
    generate.add_argument("--output", type=Path, required=True)
    generate.set_defaults(func=lambda a: cmd_generate(a.assets, a.tag, a.output))

    check_cmd = sub.add_parser("check", help="verify a manifest against assets.json")
    check_cmd.add_argument("manifest", type=Path)
    check_cmd.add_argument("assets", type=Path, help="the release's assets.json")
    check_cmd.add_argument("--tag", type=_tag, required=True)
    check_cmd.set_defaults(func=lambda a: cmd_check(a.manifest, a.assets, a.tag))

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
