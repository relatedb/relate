import json
import tempfile
import unittest
from pathlib import Path

from script_loader import ROOT, load

# The shared environment-class model (scripts/loadtest_env.py), consumed by
# both compare-loadtest.py and baseline-fingerprint.py.
env_mod = load("loadtest_env", ROOT / "scripts" / "loadtest_env.py")
fp_mod = load("baseline_fingerprint", ROOT / "scripts" / "baseline-fingerprint.py")


class BaselineFingerprintTests(unittest.TestCase):
    def test_pinned_reference_replaces_the_callers_own_digest(self) -> None:
        """The env pins arrive already digested; appending a second
        `@sha256:...` would stamp `name@a@b`, which is not a reference."""
        digest = "sha256:" + "e6" * 32
        self.assertEqual(
            fp_mod.pinned_reference(f"rust:1.99-bookworm@{digest}", f"rust:1.99-bookworm@{digest}"),
            f"rust:1.99-bookworm@{digest}",
        )
        self.assertEqual(
            fp_mod.pinned_reference("rust:1.99-bookworm", f"rust:1.99-bookworm@{digest}"),
            f"rust:1.99-bookworm@{digest}",
        )
        # A local-only image inspects to its Id, which has no `@` at all.
        self.assertEqual(
            fp_mod.pinned_reference("relate:local", "sha256:abc"),
            "relate:local@sha256:abc",
        )

    def test_stamp_writes_environment_from_report_config(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "report.json"
            path.write_text(
                json.dumps(
                    {
                        "config": {"ops": 20000, "warmup": 8, "workers": 4},
                        "throughput": {"ops_per_sec": 1},
                    }
                )
            )
            import subprocess
            import sys

            result = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts" / "baseline-fingerprint.py"),
                    "stamp",
                    str(path),
                    "--arch",
                    "x86_64",
                    "--build-image-pin",
                    "rust:1.88-bookworm@sha256:aaa",
                    "--run-image-pin",
                    "debian:bookworm-slim@sha256:bbb",
                    "--cpus",
                    "2",
                    "--cpu-period-us",
                    "10000",
                    "--cpu-quota-us",
                    "20000",
                    "--memory",
                    "2g",
                    "--runner",
                    "github-actions:ubuntu-24.04",
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            stamped = json.loads(path.read_text())
            self.assertEqual(stamped["environment"]["workers"], 4)
            self.assertEqual(stamped["environment"]["ops"], 20000)
            self.assertEqual(stamped["environment"]["class"], env_mod.CLASS)

    def test_stamp_tolerates_a_list_valued_ops_knob(self) -> None:
        """The bulk report's `config.ops` is the list of op ids measured."""
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bulk.json"
            path.write_text(
                json.dumps(
                    {
                        "config": {
                            "ops": ["create"],
                            "sizes": [1, 1000],
                            "reps": 200,
                            "warmup": 20,
                        },
                        "results": [],
                    }
                )
            )
            import subprocess
            import sys

            result = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts" / "baseline-fingerprint.py"),
                    "stamp",
                    str(path),
                    "--arch",
                    "x86_64",
                    "--build-image-pin",
                    "rust:1.88-bookworm@sha256:aaa",
                    "--run-image-pin",
                    "debian:bookworm-slim@sha256:bbb",
                    "--cpus",
                    "2",
                    "--cpu-period-us",
                    "10000",
                    "--cpu-quota-us",
                    "20000",
                    "--memory",
                    "2g",
                    "--runner",
                    "github-actions:ubuntu-24.04",
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            stamped = json.loads(path.read_text())
            self.assertIsNone(stamped["environment"]["ops"])
            self.assertEqual(stamped["environment"]["warmup"], 20)


if __name__ == "__main__":
    unittest.main()
