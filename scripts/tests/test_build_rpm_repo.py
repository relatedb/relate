#!/usr/bin/env python3
"""Tests for scripts/build_rpm_repo.py — the RPM package and signed
YUM/DNF repository tree for one release binary (ADR-0659).

The spec and the repo file are checked as the text rpmbuild and dnf
consume: the package puts the real binary under /usr/lib/relate with the
provenance marker beside it and a /usr/bin symlink, requires the glibc
floor it was given, and the repo file verifies the metadata signature
(repo_gpgcheck=1) rather than a package signature.

The end-to-end case builds a real rpm with a fixture signing keypair in a
sandbox repository (the trusted distribution/repo-signing-key.asc is
never overwritten) and verifies the produced repomd.xml signature against
that key. It runs where rpmbuild, createrepo_c, rpm and gpg exist (the
Fedora container of the node's acceptance) and skips otherwise.

Run: python3 scripts/tests/test_build_rpm_repo.py
"""

from __future__ import annotations

import base64
import contextlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from script_loader import ROOT, load

SCRIPT = ROOT / "scripts" / "build_rpm_repo.py"

build_mod = load("build_rpm_repo", SCRIPT)

E2E_TOOLS = ("rpmbuild", "createrepo_c", "rpm", "gpg")


def have_e2e_tools() -> bool:
    return all(shutil.which(tool) for tool in E2E_TOOLS)


class Rendering(unittest.TestCase):
    def test_glibc_floor_is_the_version_rpm_compares(self) -> None:
        self.assertEqual(build_mod.parse_glibc_floor("GLIBC_2.35"), "2.35")
        self.assertEqual(build_mod.parse_glibc_floor("GLIBC_2.17"), "2.17")

    def test_glibc_floor_rejects_a_non_glibc_symbol_version(self) -> None:
        with self.assertRaises(ValueError):
            build_mod.parse_glibc_floor("2.35")
        with self.assertRaises(ValueError):
            build_mod.parse_glibc_floor("GLIBC_2")

    def test_spec_requires_the_glibc_floor_and_lays_out_the_package(self) -> None:
        spec = build_mod.render_spec("1.2.0", "2.35", Path("/staged/relate-linux-amd64"))
        self.assertIn("Version:        1.2.0", spec)
        self.assertIn("Requires:       glibc >= 2.35", spec)
        # The real binary lives off PATH and the symlink puts it on PATH, so
        # the marker lands beside the canonicalized current_exe (ADR-0659).
        self.assertIn("/usr/lib/relate/relate", spec)
        self.assertIn("/usr/lib/relate/.relate-package-managed", spec)
        self.assertIn("ln -s /usr/lib/relate/relate %{buildroot}/usr/bin/relate", spec)
        self.assertIn("install -m 0755 /staged/relate-linux-amd64", spec)

    def test_marker_names_the_rpm_manager_and_the_dnf_upgrade_command(self) -> None:
        marker = json.loads(build_mod.marker_json())
        self.assertEqual(marker["manager"], "rpm")
        self.assertEqual(marker["upgrade"], "sudo dnf upgrade relate")

    def test_repo_file_verifies_the_metadata_signature(self) -> None:
        repo = build_mod.render_repo_file("https://relatedb.ai/rpm")
        self.assertIn("baseurl=https://relatedb.ai/rpm/$basearch", repo)
        self.assertIn("repo_gpgcheck=1", repo)
        self.assertIn("gpgcheck=0", repo)
        self.assertIn("gpgkey=https://relatedb.ai/rpm/repo-signing-key.asc", repo)


class Refusals(unittest.TestCase):
    def test_an_empty_signing_secret_is_refused_before_building(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "relate-linux-amd64"
            binary.write_bytes(b"#!/bin/sh\n")
            with mock.patch.dict(os.environ, {"REPO_GPG_KEY": ""}):
                with self.assertRaisesRegex(ValueError, "REPO_GPG_KEY"):
                    build_mod.build(binary, "1.2.0", "GLIBC_2.35", Path(tmp) / "out", "u")

    def test_a_version_that_is_not_xyz_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "relate-linux-amd64"
            binary.write_bytes(b"#!/bin/sh\n")
            with mock.patch.dict(os.environ, {"REPO_GPG_KEY": "x"}):
                with self.assertRaisesRegex(ValueError, "X.Y.Z"):
                    build_mod.build(binary, "v1.2.0", "GLIBC_2.35", Path(tmp) / "out", "u")

    def test_a_missing_build_tool_is_named(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "relate-linux-amd64"
            binary.write_bytes(b"#!/bin/sh\n")
            with mock.patch.object(build_mod.shutil, "which", return_value=None), \
                    mock.patch.dict(os.environ, {"REPO_GPG_KEY": "x"}):
                with self.assertRaisesRegex(ValueError, "rpmbuild"):
                    build_mod.build(binary, "1.2.0", "GLIBC_2.35", Path(tmp) / "out", "u")

    def test_a_missing_binary_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.dict(os.environ, {"REPO_GPG_KEY": "x"}):
                with self.assertRaisesRegex(ValueError, "does not exist"):
                    build_mod.build(
                        Path(tmp) / "absent", "1.2.0", "GLIBC_2.35", Path(tmp) / "out", "u"
                    )


@unittest.skipUnless(have_e2e_tools(), "rpmbuild/createrepo_c/rpm/gpg not available")
class EndToEnd(unittest.TestCase):
    """A real rpm and repository tree, signed with a fixture keypair."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix="relate-rpm-test-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

        # A fixture keypair stands in for the production key inside a
        # sandbox repository the script resolves as its own ROOT, so the
        # committed public key is never overwritten.
        self.gnupg = self.root / "gnupg"
        self.gnupg.mkdir(mode=0o700)
        subprocess.run(
            [
                "gpg", "--homedir", str(self.gnupg), "--batch", "--passphrase", "",
                "--pinentry-mode", "loopback", "--quick-generate-key",
                "relate-repo-signing-test", "rsa2048", "sign", "never",
            ],
            check=True,
            capture_output=True,
        )
        self.fingerprint = self._secret_keys()["fpr"][0].split(":")[9]

    def _gpg(self, *args: str) -> str:
        return subprocess.run(
            ["gpg", "--homedir", str(self.gnupg), "--batch", *args],
            check=True,
            capture_output=True,
            text=True,
        ).stdout

    def _secret_keys(self) -> dict[str, list[str]]:
        records: dict[str, list[str]] = {}
        for line in self._gpg("--with-colons", "--list-secret-keys").splitlines():
            records.setdefault(line.split(":")[0], []).append(line)
        return records

    def _sandbox(self) -> tuple[Path, str]:
        repo = self.root / "repo"
        (repo / "scripts").mkdir(parents=True)
        (repo / "distribution").mkdir()
        shutil.copy2(build_mod.SIGN_SCRIPT, repo / "scripts" / "sign-repo-metadata.sh")
        (repo / "distribution" / "repo-signing-key.asc").write_text(
            self._gpg("--armor", "--export", self.fingerprint)
        )
        (repo / "distribution" / "repo-signing-key.fingerprint").write_text(
            self.fingerprint + "\n"
        )
        secret = self._gpg("--armor", "--export-secret-keys", self.fingerprint)
        return repo, base64.b64encode(secret.encode()).decode()

    def test_builds_a_signed_repo_whose_package_requires_the_glibc_floor(self) -> None:
        repo, secret = self._sandbox()
        binary = self.root / "relate-linux-amd64"
        binary.write_bytes(b"#!/bin/sh\necho relate\n")
        output = self.root / "out"

        with mock.patch.object(build_mod, "SIGN_SCRIPT", repo / "scripts" / "sign-repo-metadata.sh"), \
                mock.patch.object(build_mod, "PUBLIC_KEY", repo / "distribution" / "repo-signing-key.asc"), \
                mock.patch.dict(os.environ, {"REPO_GPG_KEY": secret}), \
                contextlib.redirect_stdout(io.StringIO()):
            repo_file = build_mod.build(
                binary, "1.2.0", "GLIBC_2.35", output, "https://relatedb.ai/rpm"
            )

        arch = subprocess.run(
            ["rpm", "--eval", "%{_arch}"], check=True, capture_output=True, text=True
        ).stdout.strip()
        rpms = list((output / arch).glob("relate-1.2.0-1.*.rpm"))
        self.assertEqual(len(rpms), 1, f"expected one rpm, found {rpms}")

        requires = subprocess.run(
            ["rpm", "-qp", "--requires", str(rpms[0])],
            check=True, capture_output=True, text=True,
        ).stdout
        self.assertIn("glibc >= 2.35", requires)

        files = subprocess.run(
            ["rpm", "-qlp", str(rpms[0])], check=True, capture_output=True, text=True
        ).stdout.split()
        for owned in ("/usr/lib/relate/relate", "/usr/lib/relate/.relate-package-managed", "/usr/bin/relate"):
            self.assertIn(owned, files)

        # The tree serves the committed public key itself — the file the
        # repo's gpgkey= points at is the trust anchor, byte for byte.
        self.assertEqual(
            (output / "repo-signing-key.asc").read_bytes(),
            (repo / "distribution" / "repo-signing-key.asc").read_bytes(),
        )

        # The consumer's trust anchor is the committed public key: the
        # produced signature must verify against it.
        verifier = self.root / "verifier"
        verifier.mkdir(mode=0o700)
        subprocess.run(
            ["gpg", "--homedir", str(verifier), "--batch", "--import",
             str(output / "repo-signing-key.asc")],
            check=True, capture_output=True,
        )
        subprocess.run(
            ["gpg", "--homedir", str(verifier), "--batch", "--verify",
             str(output / arch / "repodata" / "repomd.xml.asc"),
             str(output / arch / "repodata" / "repomd.xml")],
            check=True, capture_output=True,
        )
        self.assertTrue(repo_file.is_file())


if __name__ == "__main__":
    unittest.main(verbosity=2)
