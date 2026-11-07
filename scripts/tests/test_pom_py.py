import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
POM_PY = ROOT / "scripts" / "lib" / "pom.py"

# A pom shaped like the real one: default xmlns, comments that must
# survive a set byte-for-byte, and a dependency whose <version> must
# never be the rewrite target.
POM = """<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.relate</groupId>
  <artifactId>relate</artifactId>
  <version>0.1.0</version>
  <name>relate</name>
  <!-- a comment that ElementTree serialization would drop -->
  <dependencies>
    <dependency>
      <groupId>net.java.dev.jna</groupId>
      <artifactId>jna</artifactId>
      <version>5.14.0</version>
    </dependency>
  </dependencies>
</project>
"""


def invoke(*args: str) -> subprocess.CompletedProcess[str]:
    """Run pom.py with exactly these argv words (no temp pom involved)."""
    return subprocess.run(
        [sys.executable, str(POM_PY), *args],
        capture_output=True,
        text=True,
    )


def run(
    verb: str, *args: str, pom_text: str = POM
) -> tuple[subprocess.CompletedProcess[str], str]:
    """Run ``<verb> <pom> [<args>...]`` on a temp pom.

    The path is spliced in after the verb, the shape pom.py's own usage
    spells; returns the completed process and the post-run file text.
    """
    with tempfile.NamedTemporaryFile("w", suffix=".xml", delete=False) as fh:
        fh.write(pom_text)
        path = fh.name
    try:
        return invoke(verb, path, *args), Path(path).read_text()
    finally:
        Path(path).unlink()


class GetTests(unittest.TestCase):
    def test_get_prints_the_project_version(self) -> None:
        result, _ = run("get")
        self.assertEqual(result.stdout.strip(), "0.1.0")

    def test_get_fails_loudly_without_a_project_version(self) -> None:
        result, _ = run("get", pom_text="<project><name>x</name></project>\n")
        self.assertEqual(result.returncode, 1)
        self.assertIn("no project <version>", result.stderr)

    def test_get_fails_loudly_on_malformed_xml(self) -> None:
        result, _ = run("get", pom_text="not xml at all\n")
        self.assertEqual(result.returncode, 1)
        self.assertIn("not well-formed", result.stderr)


class SetTests(unittest.TestCase):
    def test_set_rewrites_exactly_the_project_version_line(self) -> None:
        result, text = run("set", "0.16.2")
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = POM.replace(
            "  <version>0.1.0</version>", "  <version>0.16.2</version>"
        )
        # Comments and the dependency's own <version> survive intact.
        self.assertEqual(text, expected)

    def test_set_without_a_version_argument_is_a_usage_error(self) -> None:
        result, _ = run("set", pom_text="<project><name>x</name></project>\n")
        self.assertEqual(result.returncode, 1)
        self.assertIn("usage:", result.stderr)


class UsageTests(unittest.TestCase):
    def test_no_arguments_prints_usage(self) -> None:
        result = invoke()
        self.assertEqual(result.returncode, 1)
        self.assertIn("usage:", result.stderr)


if __name__ == "__main__":
    unittest.main()
