import os
import stat
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

    def test_get_treats_a_blank_version_as_absent(self) -> None:
        result, _ = run("get", pom_text="<project><version>  </version></project>\n")
        self.assertEqual(result.returncode, 1)
        self.assertIn("no project <version>", result.stderr)

    def test_get_fails_loudly_when_the_root_is_not_project(self) -> None:
        result, _ = run("get", pom_text="<pom><version>0.1.0</version></pom>\n")
        self.assertEqual(result.returncode, 1)
        self.assertIn("root element is <pom>, not <project>", result.stderr)

    def test_get_fails_loudly_on_malformed_xml(self) -> None:
        result, _ = run("get", pom_text="not xml at all\n")
        self.assertEqual(result.returncode, 1)
        self.assertIn("not well-formed", result.stderr)

    def test_a_missing_pom_is_a_diagnostic_not_a_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            missing = Path(tmp) / "absent.xml"
            result = invoke("get", str(missing))
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{missing}: cannot read", result.stderr)
        self.assertNotIn("Traceback", result.stderr)


class SetTests(unittest.TestCase):
    def test_set_rewrites_exactly_the_project_version_line(self) -> None:
        result, text = run("set", "0.16.2")
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = POM.replace(
            "  <version>0.1.0</version>", "  <version>0.16.2</version>"
        )
        # Comments and the dependency's own <version> survive intact.
        self.assertEqual(text, expected)

    def test_set_leaves_no_sidecar_beside_the_pom(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            pom = Path(tmp) / "pom.xml"
            pom.write_text(POM)
            result = invoke("set", str(pom), "0.16.2")
            self.assertEqual(result.returncode, 0, result.stderr)
            # The write lands through a sibling temp + rename; nothing is left.
            self.assertEqual([entry.name for entry in Path(tmp).iterdir()], ["pom.xml"])

    def test_set_that_finds_no_line_leaves_the_pom_untouched(self) -> None:
        # The parse finds the version, but it is not at the 2-space indent
        # the rewrite anchors on: the file must survive byte-for-byte.
        text = '<project>\n    <version>0.1.0</version>\n</project>\n'
        result, after = run("set", "0.16.2", pom_text=text)
        self.assertEqual(result.returncode, 1)
        self.assertIn("no 2-space", result.stderr)
        self.assertEqual(after, text)

    def test_set_refuses_a_version_that_is_not_element_text(self) -> None:
        # The XML-text rule, both arms: `a<b`, `a&b`, and a control character
        # XML forbids fail to parse; `&#65;` and `<a/>` parse but do not
        # round-trip to the value they were given.
        for bad in ("a<b", "a&b", "\x01", "&#65;", "<a/>"):
            with self.subTest(bad=bad):
                result = invoke("set", "pom.xml", bad)
                self.assertEqual(result.returncode, 2)
                self.assertIn("that is XML text", result.stderr)

    def test_set_refuses_a_blank_or_whitespace_version(self) -> None:
        # The CLI's own value rule: a version is non-empty and unspaced.
        # argparse rejects both tables before the pom is ever opened, so the
        # path is never read and needs no fixture.
        for bad in ("", "1 0", "1.0 "):
            with self.subTest(bad=bad):
                result = invoke("set", "pom.xml", bad)
                self.assertEqual(result.returncode, 2)
                self.assertIn("no whitespace", result.stderr)

    def test_a_greater_than_sign_is_legal_element_text(self) -> None:
        # Only `<` and `&` must be escaped in XML text, so `>` lands as the
        # element's text and is not refused.
        result, text = run("set", "1.0>b")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("  <version>1.0>b</version>", text)

    def test_set_without_a_version_argument_is_a_usage_error(self) -> None:
        # argparse rejects the missing positional before the pom is ever
        # read, so the path is never opened and needs no fixture.
        result = invoke("set", "pom.xml")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)


@unittest.skipUnless(
    hasattr(os, "geteuid") and os.geteuid() != 0,
    "a root user writes into a read-only directory",
)
class WriteFailureTests(unittest.TestCase):
    def test_an_unwritable_directory_is_a_diagnostic_not_a_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            pom = Path(tmp) / "pom.xml"
            pom.write_text(POM)
            # The sidecar temp is created beside the target, so a read-only
            # directory fails the write the way a full disk or a bad mode
            # would — as a diagnostic, not a traceback.
            original = stat.S_IMODE(os.stat(tmp).st_mode)
            os.chmod(tmp, 0o555)
            try:
                result = invoke("set", str(pom), "0.16.2")
            finally:
                os.chmod(tmp, original)
            self.assertEqual(result.returncode, 1)
            self.assertIn(f"{pom}: cannot write", result.stderr)
            self.assertNotIn("Traceback", result.stderr)
            self.assertEqual(pom.read_text(), POM)


class UsageTests(unittest.TestCase):
    def test_no_arguments_prints_usage(self) -> None:
        result = invoke()
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)

    def test_an_unknown_verb_prints_usage(self) -> None:
        result = invoke("delete", "pom.xml")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)


if __name__ == "__main__":
    unittest.main()
