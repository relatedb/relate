#!/usr/bin/env python3
"""winget manifests for the signed Windows CLI: generate the three-file
manifest set from the release asset manifest, and submit the version bump
to microsoft/winget-pkgs.

Usage:
  winget_manifests.py generate <assets.json> --version <X.Y.Z> --output <dir>
  winget_manifests.py submit <manifest-dir> [--token <token>] [--dry-run]

`generate` reads the release's asset manifest (assets.json, asset name to
sha256 and size) and writes the version, installer and default-locale
manifests winget requires — they must sit together in one directory for
`winget validate --manifest <dir>`. The installer is the bare signed
release asset `relate-windows-amd64.exe`, installed as a portable package:
its URL is the release download URL for the tag `v<version>`, and its
sha256 is the asset manifest's entry for that asset — the manifest has no
entry, or a malformed one, fails the generation rather than fabricating a
hash (ADR-0659 §4). `winget validate` in CI is the schema gate.

`submit` hands the generated manifests to wingetcreate, which forks
microsoft/winget-pkgs and opens the version-bump PR. `--dry-run` prints
the submission command and stops before submitting. The token is
`--token` or the WINGET_TOKEN environment variable.

Run: python3 scripts/winget_manifests.py generate release/assets.json \\
       --version 0.16.2 --output winget-manifests
     winget validate --manifest winget-manifests
     python3 scripts/winget_manifests.py submit winget-manifests --dry-run
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))

from release_assets import asset_sha256, load_json_object, release_download_url
from release_version import release_tag

# The winget identity of the CLI. The identifier is <Publisher>.<PackageName>
# in PascalCase, and the manifests' filenames derive from it.
PACKAGE_ID = "Relatedb.Relate"
PUBLISHER = "Relate"
PACKAGE_NAME = "Relate"

# The Windows CLI release asset (the CLI job's `relate-windows-amd64`
# artifact, renamed with its .exe extension).
ASSET_NAME = "relate-windows-amd64.exe"

# The winget manifest schema the generated files declare.
MANIFEST_VERSION = "1.10.0"
SCHEMA_URL = f"https://aka.ms/winget-manifest.{{kind}}.{MANIFEST_VERSION}.schema.json"

# wingetcreate is the official manifest submitter: it forks
# microsoft/winget-pkgs, commits the manifests and opens the PR.
SUBMITTER = "wingetcreate"
TOKEN_ENV = "WINGET_TOKEN"


class ManifestError(Exception):
    """A release asset manifest that cannot produce a winget manifest."""


def installer_url(version: str) -> str:
    """The release asset URL the manifest points at: the tagged asset on
    the release download route, never a mirror or a latest alias."""
    return release_download_url(release_tag(version), ASSET_NAME)


def manifest_files(version: str, sha256: str) -> dict[str, str]:
    """The three winget manifest files, keyed by filename."""
    header = f"# yaml-language-server: $schema={SCHEMA_URL}"

    version_manifest = f"""\
{header.format(kind="version")}
PackageIdentifier: {PACKAGE_ID}
PackageVersion: {version}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: {MANIFEST_VERSION}
"""

    installer_manifest = f"""\
{header.format(kind="installer")}
PackageIdentifier: {PACKAGE_ID}
PackageVersion: {version}
InstallerType: portable
Commands:
  - relate
Installers:
  - Architecture: x64
    InstallerUrl: {installer_url(version)}
    InstallerSha256: {sha256}
ManifestType: installer
ManifestVersion: {MANIFEST_VERSION}
"""

    locale_manifest = f"""\
{header.format(kind="defaultLocale")}
PackageIdentifier: {PACKAGE_ID}
PackageVersion: {version}
PackageLocale: en-US
Publisher: {PUBLISHER}
PublisherUrl: https://relatedb.ai
PublisherSupportUrl: https://github.com/relatedb/relate/issues
PackageName: {PACKAGE_NAME}
PackageUrl: https://github.com/relatedb/relate
License: Apache-2.0
LicenseUrl: https://github.com/relatedb/relate/blob/main/LICENSE
ShortDescription: Embedded openCypher graph database with a single-binary CLI.
Description: Relate is an embedded, in-memory property graph engine with an openCypher query front end and a single-binary CLI.
Tags:
  - graph
  - database
  - cypher
  - cli
ManifestType: defaultLocale
ManifestVersion: {MANIFEST_VERSION}
"""

    return {
        f"{PACKAGE_ID}.yaml": version_manifest,
        f"{PACKAGE_ID}.installer.yaml": installer_manifest,
        f"{PACKAGE_ID}.locale.en-US.yaml": locale_manifest,
    }


def cmd_generate(manifest_path: Path, version: str, output: Path) -> int:
    try:
        manifest = load_json_object(manifest_path, "release asset manifest", ManifestError)
        sha256 = asset_sha256(manifest, ASSET_NAME, ManifestError)
    except ManifestError as err:
        print(err, file=sys.stderr)
        return 1

    output.mkdir(parents=True, exist_ok=True)
    files = manifest_files(version, sha256)
    for name, content in files.items():
        (output / name).write_text(content)
    print(f"wrote {len(files)} winget manifests for {PACKAGE_ID} {version} to {output}")
    return 0


def submit_command(manifest_dir: Path, token: str) -> list[str]:
    """The wingetcreate invocation that opens the version-bump PR."""
    return [SUBMITTER, "submit", "--token", token, str(manifest_dir)]


def cmd_submit(manifest_dir: Path, token: str, dry_run: bool) -> int:
    if not manifest_dir.is_dir():
        print(f"manifest directory '{manifest_dir}' does not exist", file=sys.stderr)
        return 1
    if dry_run:
        shown = " ".join(submit_command(manifest_dir, f"<{TOKEN_ENV}>"))
        print(f"dry run: would submit {manifest_dir} with '{shown}'")
        return 0
    if not token:
        print(
            f"no submission token: pass --token or set {TOKEN_ENV}",
            file=sys.stderr,
        )
        return 1
    result = subprocess.run(submit_command(manifest_dir, token), check=False)
    return result.returncode


def _version(value: str) -> str:
    release_tag(value, argparse.ArgumentTypeError)
    return value


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Generate and submit the winget manifests for the Windows CLI."
    )
    sub = parser.add_subparsers(dest="command", required=True)

    gen = sub.add_parser(
        "generate", help="write the winget manifests from the release asset manifest"
    )
    gen.add_argument(
        "assets",
        type=Path,
        help="the release asset manifest (assets.json)",
    )
    gen.add_argument(
        "--version",
        type=_version,
        required=True,
        help="the release version, without the v prefix",
    )
    gen.add_argument(
        "--output", type=Path, required=True, help="directory for the manifests"
    )

    submit = sub.add_parser(
        "submit", help="submit the manifests to microsoft/winget-pkgs"
    )
    submit.add_argument("manifest_dir", type=Path, help="directory of generated manifests")
    submit.add_argument(
        "--token",
        default=os.environ.get(TOKEN_ENV, ""),
        help=f"GitHub token for the winget-pkgs fork (default: ${TOKEN_ENV})",
    )
    submit.add_argument(
        "--dry-run",
        action="store_true",
        help="print the submission command and stop before submitting",
    )

    args = parser.parse_args(argv)
    if args.command == "generate":
        return cmd_generate(args.assets, args.version, args.output)
    return cmd_submit(args.manifest_dir, args.token, args.dry_run)


if __name__ == "__main__":
    sys.exit(main())
