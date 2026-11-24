#!/usr/bin/env python3
"""Tests for scripts/winget_manifests.py — the winget manifest set generated
from the release asset manifest, and the microsoft/winget-pkgs submission
with its dry-run. The fixture is the release asset manifest checked in
under fixtures/winget/ (the same one the CI validation lane generates
from), so the installer URL and sha256 assertions cover the exact bytes
that lane hands to `winget validate`.

Run: python3 scripts/tests/test_winget_manifests.py
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from script_loader import ROOT, load

SCRIPT = ROOT / "scripts" / "winget_manifests.py"
FIXTURE = ROOT / "scripts" / "tests" / "fixtures" / "winget" / "assets.json"

winget_mod = load("winget_manifests", SCRIPT)

PACKAGE_ID = "Relatedb.Relate"
WINDOWS_ASSET = "relate-windows-amd64.exe"


def fixture_manifest() -> dict:
    return json.loads(FIXTURE.read_text())


def run_main(argv: list[str]) -> tuple[int, str, str]:
    """Run the script through main(), capturing stdout and stderr."""
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = winget_mod.main(argv)
    return code, out.getvalue(), err.getvalue()


def generate(tmp: str, manifest: dict, version: str = "0.16.2") -> Path:
    """Generate the manifests for a release asset manifest into a fresh dir."""
    manifest_path = Path(tmp) / "assets.json"
    manifest_path.write_text(json.dumps(manifest))
    output = Path(tmp) / "winget-manifests"
    code, _, err = run_main(
        ["generate", str(manifest_path), "--version", version, "--output", str(output)]
    )
    assert code == 0, err
    return output


class GenerateTests(unittest.TestCase):
    def test_writes_the_three_manifests_winget_requires(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            output = generate(tmp, fixture_manifest())
            self.assertEqual(
                sorted(p.name for p in output.iterdir()),
                sorted(
                    [
                        f"{PACKAGE_ID}.yaml",
                        f"{PACKAGE_ID}.installer.yaml",
                        f"{PACKAGE_ID}.locale.en-US.yaml",
                    ]
                ),
            )
            version = (output / f"{PACKAGE_ID}.yaml").read_text()
            self.assertIn(f"PackageIdentifier: {PACKAGE_ID}", version)
            self.assertIn("PackageVersion: 0.16.2", version)
            self.assertIn("ManifestType: version", version)
            locale = (output / f"{PACKAGE_ID}.locale.en-US.yaml").read_text()
            self.assertIn("ManifestType: defaultLocale", locale)
            self.assertIn("PackageLocale: en-US", locale)

    def test_installer_url_is_the_release_asset_url(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            installer = (
                generate(tmp, fixture_manifest()) / f"{PACKAGE_ID}.installer.yaml"
            ).read_text()
            self.assertIn(
                "InstallerUrl: https://github.com/relatedb/relate/releases/"
                "download/v0.16.2/relate-windows-amd64.exe",
                installer,
            )

    def test_installer_sha256_is_the_asset_manifest_entry(self) -> None:
        sha256 = fixture_manifest()[WINDOWS_ASSET]["sha256"]
        with tempfile.TemporaryDirectory() as tmp:
            installer = (
                generate(tmp, fixture_manifest()) / f"{PACKAGE_ID}.installer.yaml"
            ).read_text()
            self.assertIn(f"InstallerSha256: {sha256}", installer)

    def test_fails_without_the_windows_asset(self) -> None:
        manifest = fixture_manifest()
        del manifest[WINDOWS_ASSET]
        with tempfile.TemporaryDirectory() as tmp:
            manifest_path = Path(tmp) / "assets.json"
            manifest_path.write_text(json.dumps(manifest))
            output = Path(tmp) / "winget-manifests"
            code, _, err = run_main(
                [
                    "generate",
                    str(manifest_path),
                    "--version",
                    "0.16.2",
                    "--output",
                    str(output),
                ]
            )
            self.assertEqual(code, 1)
            self.assertIn(WINDOWS_ASSET, err)
            self.assertFalse(output.exists())

    def test_fails_on_a_malformed_hash(self) -> None:
        manifest = fixture_manifest()
        manifest[WINDOWS_ASSET]["sha256"] = "not-a-sha256"
        with tempfile.TemporaryDirectory() as tmp:
            manifest_path = Path(tmp) / "assets.json"
            manifest_path.write_text(json.dumps(manifest))
            code, _, err = run_main(
                [
                    "generate",
                    str(manifest_path),
                    "--version",
                    "0.16.2",
                    "--output",
                    str(Path(tmp) / "winget-manifests"),
                ]
            )
            self.assertEqual(code, 1)
            self.assertIn(WINDOWS_ASSET, err)

    def test_rejects_a_malformed_version(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "winget-manifests"
            with self.assertRaises(SystemExit):
                run_main(
                    [
                        "generate",
                        str(FIXTURE),
                        "--version",
                        "0.16",
                        "--output",
                        str(output),
                    ]
                )
            self.assertFalse(output.exists())

    def test_the_ci_lane_argv_generates_and_exits_zero(self) -> None:
        """The command the validation job runs, as a process."""
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "winget-manifests"
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "generate",
                    str(FIXTURE),
                    "--version",
                    "0.16.2",
                    "--output",
                    str(output),
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(list(output.iterdir())), 3)


def recording_subprocess(calls: list[list[str]]) -> SimpleNamespace:
    """A stand-in for the script's `subprocess` module whose `run` records
    each argv in `calls` and succeeds."""

    def fake_run(argv: list[str], check: bool) -> SimpleNamespace:
        calls.append(argv)
        return SimpleNamespace(returncode=0)

    return SimpleNamespace(run=fake_run)


class SubmitTests(unittest.TestCase):
    def test_dry_run_stops_before_submitting(self) -> None:
        calls: list[list[str]] = []

        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(
            winget_mod, "subprocess", recording_subprocess(calls)
        ):
            code, out, _ = run_main(["submit", tmp, "--dry-run"])

        self.assertEqual(code, 0)
        self.assertEqual(calls, [])
        self.assertIn("dry run", out)
        self.assertIn("wingetcreate submit", out)

    def test_requires_a_token(self) -> None:
        calls: list[list[str]] = []
        with tempfile.TemporaryDirectory() as tmp, mock.patch.dict(
            os.environ, {winget_mod.TOKEN_ENV: ""}
        ), mock.patch.object(
            winget_mod, "subprocess", recording_subprocess(calls)
        ):
            code, _, err = run_main(["submit", tmp])

        self.assertEqual(code, 1)
        self.assertEqual(calls, [])
        self.assertIn(winget_mod.TOKEN_ENV, err)

    def test_submits_the_generated_manifests(self) -> None:
        calls: list[list[str]] = []

        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(
            winget_mod, "subprocess", recording_subprocess(calls)
        ):
            code, _, _ = run_main(["submit", tmp, "--token", "ghp_test"])

        self.assertEqual(code, 0)
        self.assertEqual(
            calls, [["wingetcreate", "submit", "--token", "ghp_test", tmp]]
        )

    def test_a_missing_manifest_directory_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            code, _, err = run_main(
                ["submit", str(Path(tmp) / "absent"), "--dry-run"]
            )
        self.assertEqual(code, 1)
        self.assertIn("does not exist", err)


if __name__ == "__main__":
    unittest.main()
