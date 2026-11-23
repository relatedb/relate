#!/usr/bin/env python3
"""Tests for scripts/scoop_manifest.py — the Scoop bucket manifest
generator and its release-manifest check. Fixtures are a release asset
manifest (name -> sha256 + size, the shape release_asset_manifest.py
writes) and the manifest generated from it.

Run: python3 scripts/tests/test_scoop_manifest.py
"""

from __future__ import annotations

import contextlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from script_loader import ROOT, load

SCRIPT = ROOT / "scripts" / "scoop_manifest.py"

scoop = load("scoop_manifest", SCRIPT)

TAG = "v1.2.0"
SHA = "b04051a5c1fa2ee22b15816e05b40557dd6e56cfe268ec3088da94eb9625d071"

ASSETS: dict[str, object] = {
    "relate-windows-amd64.exe": {"sha256": SHA, "size": 21_000_000},
    "SHA256SUMS": {"sha256": "a" * 64, "size": 400},
}


def write_assets(tmp: str, name: str = "assets.json", assets: dict[str, object] = ASSETS) -> Path:
    path = Path(tmp) / name
    path.write_text(json.dumps(assets))
    return path


def generate(tmp: str, tag: str = TAG) -> Path:
    output = Path(tmp) / "relate.json"
    with contextlib.redirect_stdout(io.StringIO()):
        assert scoop.main(["generate", str(write_assets(tmp)), "--tag", tag, "--output", str(output)]) == 0
    return output


def run_check(manifest: Path, assets: Path, tag: str = TAG) -> tuple[int, str]:
    err = io.StringIO()
    with contextlib.redirect_stderr(err):
        code = scoop.main(["check", str(manifest), str(assets), "--tag", tag])
    return code, err.getvalue()


class Generate(unittest.TestCase):
    def test_manifest_carries_the_release_entry(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = json.loads(generate(tmp).read_text())
        self.assertEqual(manifest["version"], "1.2.0")
        self.assertEqual(
            manifest["architecture"]["64bit"]["url"],
            "https://github.com/relatedb/relate/releases/download/v1.2.0/relate-windows-amd64.exe",
        )
        self.assertEqual(manifest["architecture"]["64bit"]["hash"], f"sha256:{SHA}")
        self.assertEqual(manifest["bin"], [["relate-windows-amd64.exe", "relate"]])

    def test_checkver_tracks_the_latest_release_and_autoupdate_the_tag(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = json.loads(generate(tmp).read_text())
        self.assertEqual(
            manifest["checkver"],
            {
                "url": "https://relatedb.ai/releases.json",
                "jsonpath": "$.latest.tag",
                "regex": r"v(\d+\.\d+\.\d+)",
            },
        )
        update = manifest["autoupdate"]["architecture"]["64bit"]
        self.assertEqual(
            update["url"],
            "https://github.com/relatedb/relate/releases/download/v$version/relate-windows-amd64.exe",
        )
        self.assertEqual(
            update["hash"],
            {
                "url": "$baseurl/assets.json",
                "jsonpath": "$['relate-windows-amd64.exe'].sha256",
            },
        )

    def test_post_install_writes_the_provenance_marker(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = json.loads(generate(tmp).read_text())
        script = manifest["post_install"]
        self.assertIn(".relate-package-managed", script)
        self.assertIn('"manager":"scoop"', script)
        self.assertIn('"upgrade":"scoop update relate"', script)

    def test_missing_windows_asset_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            assets = write_assets(tmp, assets={"SHA256SUMS": {"sha256": "a" * 64}})
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                code = scoop.main(
                    ["generate", str(assets), "--tag", TAG, "--output", str(Path(tmp) / "m.json")]
                )
        self.assertEqual(code, 1)
        self.assertIn("relate-windows-amd64.exe", err.getvalue())

    def test_rejects_a_non_release_tag(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(SystemExit):
                scoop.main(
                    ["generate", str(write_assets(tmp)), "--tag", "1.2.0", "--output", str(Path(tmp) / "m.json")]
                )


class Check(unittest.TestCase):
    def test_generated_manifest_passes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = generate(tmp)
            code, err = run_check(manifest, write_assets(tmp))
        self.assertEqual((code, err), (0, ""))

    def test_accepts_the_bare_hash_autoupdate_writes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = generate(tmp)
            data = json.loads(manifest.read_text())
            data["architecture"]["64bit"]["hash"] = SHA
            manifest.write_text(json.dumps(data))
            code, err = run_check(manifest, write_assets(tmp))
        self.assertEqual((code, err), (0, ""))

    def test_names_every_offender(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = generate(tmp)
            data = json.loads(manifest.read_text())
            data["version"] = "1.1.0"
            data["architecture"]["64bit"]["url"] = "https://example.invalid/relate.exe"
            data["architecture"]["64bit"]["hash"] = "sha256:" + "0" * 64
            data["bin"] = [["relate-windows-amd64.exe"]]
            manifest.write_text(json.dumps(data))
            code, err = run_check(manifest, write_assets(tmp))
        self.assertEqual(code, 1)
        for line in ("version:", "url:", "hash:", "bin:"):
            self.assertIn(line, err)

    def test_names_a_broken_autoupdate_contract(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = generate(tmp)
            data = json.loads(manifest.read_text())
            data["checkver"] = {"url": "https://example.invalid"}
            data["autoupdate"]["architecture"]["64bit"]["hash"] = {"url": "$baseurl/SHA256SUMS"}
            manifest.write_text(json.dumps(data))
            code, err = run_check(manifest, write_assets(tmp))
        self.assertEqual(code, 1)
        self.assertIn("checkver:", err)
        self.assertIn("autoupdate hash url:", err)

    def test_missing_asset_entry_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = generate(tmp)
            code, err = run_check(manifest, write_assets(tmp, assets={}))
        self.assertEqual(code, 1)
        self.assertIn("relate-windows-amd64.exe", err)


class Process(unittest.TestCase):
    """The command line the CI lane runs, against the generated manifest."""

    def test_generate_then_check_exits_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = Path(tmp) / "relate.json"
            subprocess.run(
                [sys.executable, str(SCRIPT), "generate", str(write_assets(tmp)), "--tag", TAG, "--output", str(manifest)],
                check=True,
                capture_output=True,
            )
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "check", str(manifest), str(write_assets(tmp)), "--tag", TAG],
                capture_output=True,
                text=True,
            )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
