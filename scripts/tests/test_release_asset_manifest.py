#!/usr/bin/env python3
"""Tests for scripts/release_asset_manifest.py — the release asset index
(asset name to sha256 and size), its coverage gate and the post-signing
anchor gate. The fixtures are staged release directories built in temp
dirs: a complete fixture whose manifest covers every asset (check passes),
and a fixture whose asset list has one entry the manifest lacks (check
fails, naming the asset). The anchor gate is exercised against a
SHA256SUMS whose assets.json line carries the manifest's own digest, a
digest that does not, and no line at all.

Run: python3 scripts/tests/test_release_asset_manifest.py
"""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from script_loader import ROOT, capture_stderr, load

SCRIPT = ROOT / "scripts" / "release_asset_manifest.py"

manifest_mod = load("release_asset_manifest", SCRIPT)

# A staged release the way release.yml's create-release job shapes it:
# CLI binaries, a wheel, the checksums signature and the notices. The
# SHA256SUMS itself is written by write_anchor, the one builder, so every
# fixture's checksum set comes from the same place.
ASSETS: dict[str, bytes] = {
    "relate-linux-x64": b"elf bytes",
    "relate-darwin-arm64": b"mach-o bytes",
    "relate_db-0.16.2-py3-none-any.whl": b"zip bytes",
    "SHA256SUMS.sig": b"signature bytes",
    "THIRDPARTY-NOTICES.md": b"notices",
}

# The digest the staged SHA256SUMS carries until write_anchor rewrites it:
# the manifest cannot hash itself, so the pre-generate line is a stand-in.
PLACEHOLDER_DIGEST = "f" * 64


def stage_release(tmp: str, name: str = "release") -> Path:
    """Write the fixture assets into a fresh staged release directory, its
    SHA256SUMS through write_anchor."""
    release_dir = Path(tmp) / name
    release_dir.mkdir()
    for asset, content in ASSETS.items():
        (release_dir / asset).write_bytes(content)
    write_anchor(release_dir, PLACEHOLDER_DIGEST)
    return release_dir


def generate(release_dir: Path) -> Path:
    """Generate the fixture's manifest through the shared capture, swallowing
    the progress line. Returns the manifest path."""
    with contextlib.redirect_stdout(io.StringIO()):
        code, _ = capture_stderr(manifest_mod.main, ["generate", str(release_dir)])
    assert code == 0
    return release_dir / "assets.json"


def run_verb(verb: str, release_dir: Path, manifest_path: Path) -> tuple[int, str]:
    """Run one manifest verb through main() — the code path the workflow
    runs — capturing stderr, where offenders are named."""
    return capture_stderr(
        manifest_mod.main, [verb, str(release_dir), str(manifest_path)]
    )


def write_anchor(
    release_dir: Path, digest: str | None = None, *, include_manifest: bool = True
) -> Path:
    """(Re)write the staged SHA256SUMS: its assets.json line carries `digest`,
    the manifest's own sha256 by default. `include_manifest=False` leaves the
    line out — the missing-line fixture. Returns the manifest path."""
    manifest_path = release_dir / "assets.json"
    if digest is None:
        digest = hashlib.sha256(manifest_path.read_bytes()).hexdigest()
    manifest_line = f"{digest}  assets.json\n" if include_manifest else ""
    (release_dir / "SHA256SUMS").write_bytes(
        b"# relate-release-version: v0.16.2\n"
        + manifest_line.encode()
        + b"a" * 64
        + b"  relate-linux-x64\n"
    )
    return manifest_path


def drop_checksum_set(release_dir: Path) -> None:
    """Remove the staged checksum set (SHA256SUMS and its signature): the
    no-checksum-set fixture both verbs' absence cases build."""
    for name in ("SHA256SUMS", "SHA256SUMS.sig"):
        (release_dir / name).unlink()


class Generate(unittest.TestCase):
    """The generated index: JSON, one entry per staged asset, each entry
    the asset's sha256 and size — and never an entry for the manifest
    itself."""

    def test_covers_every_asset_with_sha256_and_size(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            manifest_path = generate(release_dir)
            manifest = json.loads(manifest_path.read_text())
            staged = {p.name for p in release_dir.iterdir() if p.name != "assets.json"}
            self.assertEqual(set(manifest), staged)
            for name in staged:
                content = (release_dir / name).read_bytes()
                self.assertEqual(
                    manifest[name],
                    {
                        "sha256": hashlib.sha256(content).hexdigest(),
                        "size": len(content),
                    },
                )
            # The manifest rides the release as an asset but cannot carry
            # its own digest: no self-entry.
            self.assertNotIn("assets.json", manifest)

    def test_no_assets_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp) / "release"
            empty.mkdir()
            code, err = capture_stderr(manifest_mod.main, ["generate", str(empty)])
            self.assertEqual(code, 1)
            self.assertIn("no assets", err)


class Check(unittest.TestCase):
    """The gate: zero on the complete fixture, non-zero naming the
    offender whenever the manifest and the staged assets disagree."""

    def test_complete_fixture_passes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            generate(release_dir)
            code, err = run_verb("check", release_dir, release_dir / "assets.json")
            self.assertEqual(code, 0)
            self.assertEqual(err, "")

    def test_manifest_lacking_one_entry_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            generate(release_dir)
            manifest_path = release_dir / "assets.json"
            manifest = json.loads(manifest_path.read_text())
            del manifest["relate-linux-x64"]
            manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True))
            code, err = run_verb("check", release_dir, manifest_path)
            self.assertEqual(code, 1)
            self.assertIn("relate-linux-x64: no manifest entry", err)

    def test_entry_not_matching_the_asset_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            generate(release_dir)
            # An asset swapped after the manifest was written.
            (release_dir / "relate-linux-x64").write_bytes(b"swapped bytes")
            code, err = run_verb("check", release_dir, release_dir / "assets.json")
            self.assertEqual(code, 1)
            self.assertIn("relate-linux-x64: manifest sha256/size", err)

    def test_entry_for_an_unstaged_asset_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            generate(release_dir)
            manifest_path = release_dir / "assets.json"
            manifest = json.loads(manifest_path.read_text())
            manifest["relate-windows-x64.exe"] = {
                "sha256": "0" * 64,
                "size": 1,
            }
            manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True))
            code, err = run_verb("check", release_dir, manifest_path)
            self.assertEqual(code, 1)
            self.assertIn("relate-windows-x64.exe: manifest entry", err)

    def test_entry_of_the_wrong_type_is_a_distinct_refusal(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            generate(release_dir)
            manifest_path = release_dir / "assets.json"
            manifest = json.loads(manifest_path.read_text())
            # A present key of the wrong type is a malformed entry, never a
            # missing one: JSON null reads as no entry under a `.get()`.
            manifest["relate-linux-x64"] = None
            manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True))
            code, err = run_verb("check", release_dir, manifest_path)
            self.assertEqual(code, 1)
            self.assertIn("relate-linux-x64: manifest entry is malformed", err)
            self.assertNotIn("relate-linux-x64: no manifest entry", err)

    def test_entry_with_a_wrong_typed_field_is_malformed(self) -> None:
        # Every entry of the wrong shape is refused, so no Entry can hold a
        # wrong-typed field: a non-string sha256, a missing one, a non-integer
        # size, and a bool (which isinstance(size, int) would admit) all fail.
        wrong = {
            "sha256 not a string": {"sha256": 123, "size": 4},
            "sha256 missing": {"size": 4},
            "size not an integer": {"sha256": "0" * 64, "size": "4"},
            "size a bool": {"sha256": "0" * 64, "size": True},
        }
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            generate(release_dir)
            manifest_path = release_dir / "assets.json"
            for label, entry in wrong.items():
                with self.subTest(label):
                    manifest = json.loads(manifest_path.read_text())
                    manifest["relate-linux-x64"] = entry
                    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True))
                    code, err = run_verb("check", release_dir, manifest_path)
                    self.assertEqual(code, 1)
                    self.assertIn("relate-linux-x64: manifest entry is malformed", err)

    def test_empty_release_dir_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp) / "release"
            empty.mkdir()
            manifest_path = Path(tmp) / "assets.json"
            manifest_path.write_text("{}")
            code, err = run_verb("check", empty, manifest_path)
            self.assertEqual(code, 1)
            self.assertIn("no assets", err)

    def test_check_runs_before_the_checksum_set_exists(self) -> None:
        # The pre-sums gate: the manifest is generated and checked before
        # SHA256SUMS exists, because the checksum set covers the manifest.
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            drop_checksum_set(release_dir)
            generate(release_dir)
            code, err = run_verb("check", release_dir, release_dir / "assets.json")
            self.assertEqual((code, err), (0, ""))


class Anchor(unittest.TestCase):
    """The post-signing gate: SHA256SUMS must carry the manifest's digest,
    not merely a line naming it."""

    def test_matching_digest_passes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            generate(release_dir)
            manifest_path = write_anchor(release_dir)
            self.assertEqual(run_verb("anchor", release_dir, manifest_path), (0, ""))

    def test_other_digest_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            generate(release_dir)
            manifest_path = write_anchor(release_dir, "0" * 64)
            code, err = run_verb("anchor", release_dir, manifest_path)
            self.assertEqual(code, 1)
            self.assertIn("does not match", err)
            self.assertIn(hashlib.sha256(manifest_path.read_bytes()).hexdigest(), err)

    def test_missing_line_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            generate(release_dir)
            manifest_path = write_anchor(release_dir, include_manifest=False)
            code, err = run_verb("anchor", release_dir, manifest_path)
            self.assertEqual(code, 1)
            self.assertIn("carries no line for the manifest", err)

    def test_missing_checksum_set_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            generate(release_dir)
            drop_checksum_set(release_dir)
            code, err = run_verb("anchor", release_dir, release_dir / "assets.json")
            self.assertEqual(code, 1)
            self.assertIn("checksum set", err)
            self.assertIn("does not exist", err)


class RequiredInputs(unittest.TestCase):
    """A missing required input is one refusal spelling, whichever verb and
    whichever input is missing: check and anchor call the same helper."""

    def test_missing_manifest_and_checksum_set_share_one_refusal(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            release_dir = stage_release(tmp)
            generate(release_dir)
            code, err = run_verb("check", release_dir, Path(tmp) / "absent.json")
            self.assertEqual(code, 1)
            self.assertIn("manifest '", err)
            self.assertIn("' does not exist", err)
            drop_checksum_set(release_dir)
            code, err = run_verb("anchor", release_dir, release_dir / "assets.json")
            self.assertEqual(code, 1)
            self.assertIn("checksum set '", err)
            self.assertIn("' does not exist", err)


class WorkflowStep(unittest.TestCase):
    """The release.yml gate step as the release runs it: a process whose
    exit status is the workflow step's — zero on the complete fixture,
    non-zero on the one missing an entry."""

    def run_step(self, release_dir: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "check",
                str(release_dir),
                str(release_dir / "assets.json"),
            ],
            capture_output=True,
            text=True,
        )

    def test_gate_step_exit_status_on_both_fixtures(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            complete = stage_release(tmp)
            generate(complete)
            self.assertEqual(self.run_step(complete).returncode, 0)

            incomplete = stage_release(tmp, "incomplete")
            generate(incomplete)
            manifest_path = incomplete / "assets.json"
            manifest = json.loads(manifest_path.read_text())
            del manifest["relate-linux-x64"]
            manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True))
            failed = self.run_step(incomplete)
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("relate-linux-x64: no manifest entry", failed.stderr)


if __name__ == "__main__":
    unittest.main()
