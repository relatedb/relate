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
                        "optionalDependencies": {"@relate/a": "1.0.1", "@relate/b": "0.9.0"},
                    },
                    "node_modules/koffi": {"version": "2.0.0"},
                },
            },
            indent,
        )

    def test_version_is_indent_independent(self):
        for indent in (2, 4, None):
            self.assertEqual(run("version", self.lock(indent)).stdout.strip(), "1.0.0")

    def test_root_version_ignores_other_packages(self):
        self.assertEqual(run("root-version", self.lock(4)).stdout.strip(), "1.0.1")

    def test_pins_counts_matching_relate_deps_in_lockfile_root(self):
        self.assertEqual(run("lock-pins", self.lock(), "1.0.1").stdout.strip(), "1")

    def test_pins_counts_manifest_deps(self):
        path = self.write({"version": "1", "optionalDependencies": {"@relate/a": "1", "left": "1"}})
        self.assertEqual(run("pins", path, "1").stdout.strip(), "1")

    def test_lockfile_without_packages_fails_not_zero(self):
        res = run("lock-pins", self.write({"version": "1"}), "1")
        self.assertNotEqual(res.returncode, 0)

    def test_manifest_without_optional_dependencies_fails_not_zero(self):
        res = run("pins", self.write({"version": "1"}), "1")
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
        res = run("pins", path, "1")
        self.assertNotEqual(res.returncode, 0)
        self.assertIn("is not a string", res.stderr)


if __name__ == "__main__":
    unittest.main()
