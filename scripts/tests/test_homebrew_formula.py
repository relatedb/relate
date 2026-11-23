#!/usr/bin/env python3
"""Tests for scripts/homebrew_formula.py — the Homebrew formula renderer that
pins, for every platform Homebrew serves, the release URL and the release
asset manifest's sha256, and writes the result into the tap checkout.

The fixtures are release asset manifests shaped like assets.json (asset name
to sha256 and size) in temp directories.

Run: python3 scripts/tests/test_homebrew_formula.py
"""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import re
import tempfile
import unittest
from pathlib import Path

from script_loader import ROOT, load

SCRIPT = ROOT / "scripts" / "homebrew_formula.py"

formula_mod = load("homebrew_formula", SCRIPT)

TAG = "v1.2.0"

# The four assets Homebrew serves, and two more the release attaches for
# other channels: entries the formula must never mention.
PLATFORM_ASSETS = {
    "relate-darwin-arm64": b"mach-o arm64 bytes",
    "relate-darwin-amd64": b"mach-o amd64 bytes",
    "relate-linux-arm64": b"elf arm64 bytes",
    "relate-linux-amd64": b"elf amd64 bytes",
}
OTHER_ASSETS = {
    "relate-windows-amd64.exe": b"pe bytes",
    "relate_db-1.2.0-py3-none-any.whl": b"zip bytes",
}


def manifest() -> dict[str, dict[str, object]]:
    """A release asset manifest covering every Homebrew platform."""
    return {
        name: {"sha256": hashlib.sha256(content).hexdigest(), "size": len(content)}
        for name, content in {**PLATFORM_ASSETS, **OTHER_ASSETS}.items()
    }


def write_manifest(tmp: str, manifest_obj: object) -> Path:
    path = Path(tmp) / "assets.json"
    path.write_text(json.dumps(manifest_obj))
    return path


def run(argv: list[str]) -> tuple[int, str, str]:
    """Run main() the way the release step does, capturing both streams."""
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = formula_mod.main(argv)
    return code, out.getvalue(), err.getvalue()


def platforms(formula: str) -> dict[str, str]:
    """The formula's url -> sha256 pairs, keyed by the URL's asset name."""
    pairs = re.findall(r'url "(\S+)"\n\s+sha256 "([0-9a-f]{64})"', formula)
    return {url.rsplit("/", 1)[-1]: sha256 for url, sha256 in pairs}


class Render(unittest.TestCase):
    """The rendered formula pins every Homebrew platform's artifact to the
    sha256 the release manifest records, and installs it as a package."""

    def test_every_platform_carries_the_manifest_sha256(self) -> None:
        manifest_obj = manifest()
        rendered = formula_mod.render(manifest_obj, TAG)
        self.assertEqual(
            platforms(rendered),
            {
                asset: manifest_obj[asset]["sha256"]
                for asset in PLATFORM_ASSETS
            },
        )

    def test_platform_urls_ride_the_release_download_route(self) -> None:
        rendered = formula_mod.render(manifest(), TAG)
        for asset in PLATFORM_ASSETS:
            self.assertIn(
                f'url "https://github.com/relatedb/relate/releases/download/{TAG}/{asset}"',
                rendered,
            )

    def test_each_platform_sits_in_its_os_and_cpu_block(self) -> None:
        rendered = formula_mod.render(manifest(), TAG)
        expected = {
            ("macos", "arm"): "relate-darwin-arm64",
            ("macos", "intel"): "relate-darwin-amd64",
            ("linux", "arm"): "relate-linux-arm64",
            ("linux", "intel"): "relate-linux-amd64",
        }
        for (os_name, cpu), asset in expected.items():
            block = re.search(
                rf"  on_{os_name} do\n(.*?)\n  end\n", rendered, re.DOTALL
            )
            assert block is not None, os_name
            cpu_block = re.search(
                rf"    on_{cpu} do\n(.*?)\n    end", block.group(1), re.DOTALL
            )
            assert cpu_block is not None, cpu
            self.assertIn(asset, cpu_block.group(1))

    def test_assets_for_other_channels_stay_out(self) -> None:
        rendered = formula_mod.render(manifest(), TAG)
        for asset in OTHER_ASSETS:
            self.assertNotIn(asset, rendered)

    def test_install_writes_the_provenance_marker_beside_the_binary(self) -> None:
        rendered = formula_mod.render(manifest(), TAG)
        # The marker (ADR-0659) sits in the real binary's directory, not the
        # PATH symlink's, so `relate upgrade` finds it after canonicalizing.
        self.assertIn('(libexec/".relate-package-managed").write', rendered)
        self.assertIn(
            '{"manager":"homebrew","upgrade":"brew upgrade relate"}', rendered
        )
        self.assertIn('bin.install_symlink libexec/"relate"', rendered)

    def test_test_block_asserts_the_release_version(self) -> None:
        rendered = formula_mod.render(manifest(), TAG)
        self.assertIn(
            'assert_match version.to_s, shell_output("#{bin}/relate --version")',
            rendered,
        )


class RenderFailures(unittest.TestCase):
    """A formula that cannot verify a platform never renders."""

    def assert_fails(self, manifest_obj: object, tag: str, message: str) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = write_manifest(tmp, manifest_obj)
            code, out, err = run([str(path), tag, "--tap-dir", tmp])
        self.assertEqual(code, 1)
        self.assertEqual(out, "")
        self.assertIn(message, err)

    def test_missing_platform_asset_fails_naming_it(self) -> None:
        manifest_obj = manifest()
        del manifest_obj["relate-linux-amd64"]
        self.assert_fails(
            manifest_obj, TAG, "no entry for 'relate-linux-amd64'"
        )

    def test_malformed_sha256_fails(self) -> None:
        manifest_obj = manifest()
        manifest_obj["relate-darwin-arm64"]["sha256"] = "not-a-digest"
        self.assert_fails(manifest_obj, TAG, "is not 64 hex digits")

    def test_tag_outside_vX_dot_Y_dot_Z_fails(self) -> None:
        self.assert_fails(manifest(), "1.2.0", "is not vX.Y.Z")

    def test_missing_manifest_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            code, _, err = run([str(Path(tmp) / "assets.json"), TAG, "--tap-dir", tmp])
        self.assertEqual(code, 1)
        self.assertIn("does not exist", err)

    def test_manifest_that_is_not_an_object_fails(self) -> None:
        self.assert_fails([], TAG, "is not a JSON object")


class Update(unittest.TestCase):
    """Writing the tap: a dry run proves the render without touching the tap,
    the real run writes Formula/relate.rb."""

    def test_writes_the_formula_into_the_tap(self) -> None:
        manifest_obj = manifest()
        with tempfile.TemporaryDirectory() as tmp:
            path = write_manifest(tmp, manifest_obj)
            tap = Path(tmp) / "tap"
            code, out, err = run([str(path), TAG, "--tap-dir", str(tap)])
            self.assertEqual(code, 0, err)
            written = (tap / "Formula" / "relate.rb").read_text()
            self.assertEqual(err, "")
            self.assertIn(f"wrote {tap / 'Formula' / 'relate.rb'}", out)
        self.assertEqual(written, formula_mod.render(manifest_obj, TAG))

    def test_dry_run_prints_the_formula_and_writes_nothing(self) -> None:
        manifest_obj = manifest()
        with tempfile.TemporaryDirectory() as tmp:
            path = write_manifest(tmp, manifest_obj)
            tap = Path(tmp) / "tap"
            code, out, err = run(
                [str(path), TAG, "--tap-dir", str(tap), "--dry-run"]
            )
            self.assertEqual(code, 0, err)
            self.assertFalse((tap / "Formula" / "relate.rb").exists())
            self.assertFalse(tap.exists())
        self.assertIn(f"would write {tap / 'Formula' / 'relate.rb'}", out)
        self.assertIn(formula_mod.render(manifest_obj, TAG), out)


if __name__ == "__main__":
    unittest.main()
