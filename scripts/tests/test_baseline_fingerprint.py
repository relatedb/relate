import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from script_loader import ROOT, load

# The shared environment-class model (scripts/loadtest_env.py), consumed by
# both compare-loadtest.py and baseline-fingerprint.py.
env_mod = load("loadtest_env", ROOT / "scripts" / "loadtest_env.py")
fp_mod = load("baseline_fingerprint", ROOT / "scripts" / "baseline-fingerprint.py")


def run_stamp(
    path: Path,
    *extra: str,
    pins: bool = True,
    arch: str | None = "x86_64",
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """The one `stamp` invocation the tests pin — the CLI name, the argv
    shape and the class fields in one place, so a new stamp flag is added
    once and the cases cannot drift on the contract they assert. `extra`
    appends case-specific flags; `env` replaces the environment."""
    return subprocess.run(
        [
            sys.executable,
            str(ROOT / "scripts" / "baseline-fingerprint.py"),
            "stamp",
            str(path),
            # `arch=None` omits --arch, which makes the run image the
            # reference the stamp must resolve.
            *(("--arch", arch) if arch else ()),
            # The -pin flags are what the class actually records; with both
            # present and --arch given, no image reference is required
            # (REL8-2081). `pins=False` omits them for the case that must
            # resolve a reference.
            *((
                "--build-image-pin",
                "rust:1.88-bookworm@sha256:aaa",
                "--run-image-pin",
                "debian:bookworm-slim@sha256:bbb",
            ) if pins else ()),
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
            *extra,
        ],
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )


class BaselineFingerprintTests(unittest.TestCase):
    def _no_baseline_env(self) -> dict[str, str]:
        import os

        return {k: v for k, v in os.environ.items() if not k.startswith("BASELINE_")}

    def test_a_missing_build_image_is_a_loud_stop(self) -> None:
        """REL8-2081: no fabricated floating default — a stamp whose build pin
        is absent and whose reference is neither flagged nor in the
        environment (the pins home exports it) stops."""
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "report.json"
            path.write_text(json.dumps({"config": {"ops": 1}}))
            result = run_stamp(path, pins=False, env=self._no_baseline_env())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("BASELINE_BUILD_IMAGE is not set", result.stderr)

    def test_a_missing_run_image_is_a_loud_stop(self) -> None:
        """The run reference is required when --arch is absent (the arch is
        read out of the run image), even with both pins given."""
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "report.json"
            path.write_text(json.dumps({"config": {"ops": 1}}))
            result = run_stamp(path, arch=None, env=self._no_baseline_env())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("BASELINE_RUN_IMAGE is not set", result.stderr)

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
            result = run_stamp(path)
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
            result = run_stamp(path)
            self.assertEqual(result.returncode, 0, result.stderr)
            stamped = json.loads(path.read_text())
            self.assertIsNone(stamped["environment"]["ops"])
            self.assertEqual(stamped["environment"]["warmup"], 20)


if __name__ == "__main__":
    unittest.main()
