import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NPM_JSON = ROOT / "scripts" / "lib" / "npm_json.py"


def run(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run([sys.executable, str(NPM_JSON), *args], capture_output=True, text=True)


class NpmJsonTest(unittest.TestCase):
    def write(self, obj, indent=2) -> str:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        path = Path(tmp.name) / "f.json"
        path.write_text(json.dumps(obj, indent=indent))
        return str(path)

    def lock(self, indent=2):
        return self.write(
            {
                "name": "x",
                "version": "1.0.0",
                "packages": {
                    "": {
                        "version": "1.0.1",
                        "optionalDependencies": {
                            "@relate/relate-a": "file:platforms/a",
                            "@relate/relate-b": "file:platforms/b",
                        },
                        # the linked entries the lock witness reads
                        "platforms-note": None,
                    },
                    "node_modules/koffi": {"version": "2.0.0"},
                    "platforms/a": {"version": "1.0.1"},
                    "platforms/b": {"version": "1.0.1"},
                },
            },
            indent,
        )

    def test_version_is_indent_independent(self):
        for indent in (2, 4, None):
            self.assertEqual(run("version", self.lock(indent)).stdout.strip(), "1.0.0")

    def test_root_version_ignores_other_packages(self):
        self.assertEqual(run("root-version", self.lock(4)).stdout.strip(), "1.0.1")

    def test_file_links_counts_manifest_deps(self):
        path = self.write({"version": "1", "optionalDependencies": {"@relate/relate-a": "file:platforms/a", "left": "1"}})
        self.assertEqual(run("file-links", path).stdout.strip(), "1")

    def test_lock_file_links_reads_the_default_fixture(self):
        self.assertEqual(run("lock-file-links", self.lock()).stdout.strip(), "2")

    def test_file_links_refuses_a_registry_pin_loudly(self):
        # the guard: a versioned @relate pin in the repository manifest is
        # the exact shape the release-broken npm ci came from — refuse it,
        # never count it
        path = self.write({"version": "1", "optionalDependencies": {"@relate/relate-a": "1.0.3"}})
        res = run("file-links", path)
        self.assertNotEqual(res.returncode, 0)

    def test_file_links_refuses_a_crossed_link_loudly(self):
        # the ADR-0562 correspondence: a platform link must resolve the
        # package it names — a link to another platform's directory is
        # refused, never counted
        path = self.write({"version": "1", "optionalDependencies": {"@relate/relate-a": "file:platforms/b"}})
        res = run("file-links", path)
        self.assertNotEqual(res.returncode, 0)

    def test_file_links_refuses_a_non_platform_relate_dep_loudly(self):
        # @relate-scoped but not a platform package name: the scoped set
        # and the platform set are one predicate, not two
        path = self.write({"version": "1", "optionalDependencies": {"@relate/other": "file:platforms/x"}})
        res = run("file-links", path)
        self.assertNotEqual(res.returncode, 0)

    def test_lock_platform_versions_witnesses_the_linked_entries(self):
        self.assertEqual(run("lock-platform-versions", self.lock(), "1.0.1").stdout.strip(), "2")
        self.assertEqual(run("lock-platform-versions", self.lock(), "9.9.9").stdout.strip(), "0")

    def test_pin_rewrites_only_the_platform_links_to_the_manifests_version(self):
        path = self.write({
            "version": "1.0.3",
            "optionalDependencies": {
                "@relate/relate-a": "file:platforms/a",
                "left": "2.0.0",
            },
        })
        res = run("pin", path)
        self.assertEqual(res.returncode, 0)
        self.assertEqual(json.loads(res.stdout), {"@relate/relate-a": "1.0.3"})
        doc = json.loads(Path(path).read_text())
        # only the platform links are pinned; other optionals untouched
        self.assertEqual(doc["optionalDependencies"]["@relate/relate-a"], "1.0.3")
        self.assertEqual(doc["optionalDependencies"]["left"], "2.0.0")

    def test_pin_refuses_a_manifest_without_platform_links(self):
        path = self.write({"version": "1", "optionalDependencies": {"left": "1"}})
        res = run("pin", path)
        self.assertNotEqual(res.returncode, 0)

    def test_lockfile_without_packages_fails_not_zero(self):
        res = run("lock-file-links", self.write({"version": "1"}))
        self.assertNotEqual(res.returncode, 0)

    def test_manifest_without_optional_dependencies_fails_not_zero(self):
        res = run("file-links", self.write({"version": "1"}))
        self.assertNotEqual(res.returncode, 0)

    def test_missing_version_fails_loudly(self):
        res = run("version", self.write({"name": "x"}))
        self.assertNotEqual(res.returncode, 0)
        self.assertIn('no str "version"', res.stderr)

    def test_missing_root_entry_fails_loudly(self):
        res = run("root-version", self.write({"version": "1", "packages": {}}))
        self.assertNotEqual(res.returncode, 0)
        self.assertIn('no dict ""', res.stderr)

    def test_non_string_pin_value_fails_loudly(self):
        path = self.write({"optionalDependencies": {"@relate/a": 1}})
        res = run("file-links", path)
        self.assertNotEqual(res.returncode, 0)
        self.assertIn("is not a string", res.stderr)


if __name__ == "__main__":
    unittest.main()
