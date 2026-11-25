#!/usr/bin/env python3
"""Tests for the Chocolatey package's install template
(distribution/chocolatey/tools/chocolateyinstall.ps1, ADR-0659). The
template is a definitions-plus-one-entry-call PowerShell script, so these
checks are source shape — the one layer that runs on every host, beside
the Scoop and winget template tests. The Windows-only executions (the
packed package's own install) live in the publish-registries workflow;
distribution/install.ps1's harness runs on Windows alone.

Run: python3 scripts/tests/test_chocolatey_install.py
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

from script_loader import ROOT

TEMPLATE = ROOT / "distribution" / "chocolatey" / "tools" / "chocolateyinstall.ps1"
ENTRY = "Install-RelateChocolatey"
TEXT = TEMPLATE.read_text()


def position(needle: str) -> int:
    """Locate a template fragment, failing loudly when it is absent: a
    missing fragment must not compare as -1 and pass an ordering check."""
    index = TEXT.find(needle)
    if index == -1:
        raise AssertionError(f"{needle!r} missing from the template")
    return index


class TemplateShape(unittest.TestCase):
    """The install script loads as definitions, then installs once."""

    def test_definitions_then_exactly_one_entry_call(self) -> None:
        lines = [line.strip() for line in TEXT.splitlines() if line.strip()]
        self.assertEqual(lines[-1], ENTRY)
        self.assertEqual(lines.count(ENTRY), 1)
        self.assertTrue(any(line.startswith("function ") for line in lines))


class SignatureGate(unittest.TestCase):
    """A downloaded binary with no valid Authenticode signature never
    reaches the tools directory."""

    def test_signature_status_valid_is_required(self) -> None:
        self.assertIn("Get-AuthenticodeSignature", TEXT)
        self.assertRegex(TEXT, r"\$signature\.Status\s+-ne\s+'Valid'")

    def test_signature_gate_runs_before_the_install(self) -> None:
        gate = position("Assert-RelateAuthenticodeSignature -Path $downloadPath")
        self.assertLess(gate, position("Move-Item -LiteralPath $downloadPath"))
        self.assertLess(gate, position("Install-BinFile"))


class VersionGate(unittest.TestCase):
    """The package version must form the release tag the download fetches."""

    def test_version_is_validated_against_the_release_tag_grammar(self) -> None:
        self.assertIn(r"\Av[0-9]+\.[0-9]+\.[0-9]+\z", TEXT)

    def test_the_refusal_is_authored(self) -> None:
        self.assertIn("is not a vX.Y.Z tag); installing nothing", TEXT)


class Staging(unittest.TestCase):
    """The signed checksum set and the binary are staged in one fresh
    directory under %TEMP% and only the verified binary reaches the tools
    directory."""

    def test_one_fresh_staging_directory_per_install(self) -> None:
        self.assertRegex(
            TEXT, r"\$staging\s*=\s*Join-Path \$env:TEMP \(\s*'relate-'\s*\+\s*\[Guid\]::NewGuid\(\)"
        )

    def test_the_staged_inventory_is_exactly_the_checksum_set_and_the_binary(self) -> None:
        # By name, not by count: the signed checksum set, its signature, and
        # the binary — and nothing else staged under $staging.
        staged = re.findall(r"Join-Path \$staging (\S+)", TEXT)
        self.assertEqual(
            sorted(staged), sorted(["'SHA256SUMS'", "'SHA256SUMS.sig'", "$RelateAsset"])
        )
        self.assertNotIn("assets.json", TEXT)

    def test_no_predictable_download_path(self) -> None:
        self.assertNotRegex(TEXT, r"relate-assets-|relate-\$version")

    def test_staging_directory_is_removed_after_the_move(self) -> None:
        self.assertIn("Remove-Item -LiteralPath $staging", TEXT)
        self.assertLess(
            position("Move-Item"), position("Remove-Item -LiteralPath $staging")
        )

    def test_the_sha256_gate_reads_the_signed_checksum_set(self) -> None:
        # The hash is computed through .NET, not Get-FileHash: Chocolatey's
        # PowerShell host does not reliably load the Utility module.
        self.assertIn("Get-RelateFileSha256 -Path $downloadPath", TEXT)
        self.assertIn("Get-RelateSumsEntry -Sums $sums -Asset $RelateAsset", TEXT)
        self.assertIn("-ine $expected", TEXT)


class SignedChecksumSet(unittest.TestCase):
    """The binary's expected hash comes from the SSHSIG-signed SHA256SUMS,
    verified against the pinned key, never from the unsigned asset manifest."""

    def test_checksum_set_and_signature_are_downloaded(self) -> None:
        self.assertIn("/SHA256SUMS\"", TEXT)
        self.assertIn("/SHA256SUMS.sig\"", TEXT)

    def test_signature_is_verified_against_the_pinned_key(self) -> None:
        self.assertIn("-Y verify", TEXT)
        self.assertIn("allowed_signers", TEXT)
        self.assertIn("$RelateSigningKey", TEXT)

    def test_a_failed_signature_refuses_the_install(self) -> None:
        self.assertIn("SHA256SUMS signature verification failed", TEXT)
        self.assertLess(
            position("Assert-RelateManifestSignature -Keygen"), position("Move-Item")
        )

    def test_the_signed_version_header_is_bound_before_the_entry_is_read(self) -> None:
        binding = position("Assert-RelateManifestVersion -Sums $sums -Tag $tag")
        self.assertLess(binding, position("Get-RelateSumsEntry -Sums $sums"))
        self.assertLess(binding, position("Move-Item"))


class SignerPin(unittest.TestCase):
    """A Valid Authenticode signature from any certificate but the pinned one
    is refused, and a package carrying no pin installs nothing."""

    def test_signer_thumbprint_is_pinned(self) -> None:
        self.assertIn("SignerCertificate.Thumbprint", TEXT)
        self.assertIn("signer-thumbprint.txt", TEXT)

    def test_a_missing_pin_refuses_the_install(self) -> None:
        self.assertIn("pins no Authenticode signer thumbprint", TEXT)


if __name__ == "__main__":
    unittest.main()
