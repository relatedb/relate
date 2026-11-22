from __future__ import annotations

import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LIB = ROOT / "scripts" / "lib"

# One test module for both of release-bump.sh's XML version accessors: the
# machine is shared (scripts/lib/version_element.py), so the contract tests
# below run once per spec through each CLI, and each spec's own selection
# refusals — the part that genuinely differs — live in its subclass.


class AccessorContract:
    # A mixin, not a TestCase: with empty spec facts it tests nothing, so
    # unittest must not collect it — the subclasses below add the TestCase.
    """The shared machine's contract, driven through one spec's CLI.

    Subclasses set the spec's facts: the CLI, a well-formed document whose
    comments must survive a `set` byte-for-byte, the element tag, the
    indent, a wrong indent width, and the refusal matrix. The version
    line, its post-`set` form, and the wrong-indent document derive from
    those — never re-spelled per subclass.
    """

    label = ""  # the CLI file name under scripts/lib
    fixture = ""  # a well-formed document at version `old`
    old = ""
    new = ""
    element = ""  # the spec's element tag
    indent = 0  # the spec's indent width
    wrong_indent = 0  # an indent width the rewrite cannot anchor on
    # One refusal matrix per spec: (document text, expected stderr
    # substrings) pairs, asserted by the single `get`-refusals test below
    # so the assertion shape exists once — a new refusal is a row, not a
    # copied test body.
    refusals: tuple[tuple[str, tuple[str, ...]], ...] = ()

    @classmethod
    def setUpClass(cls) -> None:
        """Every spec fact is required, and its absence is loud: a
        half-filled subclass must fail here, not drift into a test that
        asserts something else or nothing at all."""
        missing = [
            name
            for name in ("label", "fixture", "old", "new", "element", "refusals")
            if not getattr(cls, name)
        ]
        if not cls.indent or not cls.wrong_indent:
            missing.append("indent/wrong_indent")
        elif cls.indent == cls.wrong_indent:
            raise AssertionError(
                f"{cls.__name__}: wrong_indent must differ from indent"
            )
        if missing:
            raise AssertionError(
                f"{cls.__name__} must set: {', '.join(missing)}"
            )

    def _line(self, version: str, pad: str) -> str:
        return f"{pad}<{self.element}>{version}</{self.element}>"

    @property
    def old_line(self) -> str:
        """The fixture's version line, derived from the spec's own facts."""
        return self._line(self.old, " " * self.indent)

    @property
    def new_line(self) -> str:
        return self._line(self.new, " " * self.indent)

    @property
    def wrong_indent_text(self) -> str:
        """The fixture with its version line moved to the spec's declared
        wrong width: the same document shape, so only the rewrite anchor
        can refuse it — never a second spelling of the shape."""
        return self.fixture.replace(
            self.old_line, self._line(self.old, " " * self.wrong_indent)
        )

    def test_get_refusals(self) -> None:
        # Labeled by row: two rows can expect the same substrings, and the
        # label must still tell them apart.
        for row, (text, expected) in enumerate(self.refusals):
            with self.subTest(row=row):
                result, _ = self.run_verb("get", text=text)
                self.assertEqual(result.returncode, 1)
                for substr in expected:
                    self.assertIn(substr, result.stderr)

    @classmethod
    def cli(cls) -> Path:
        return LIB / cls.label

    def invoke(self, *args: str) -> subprocess.CompletedProcess[str]:
        """Run the CLI with exactly these argv words (no document involved)."""
        return subprocess.run(
            [sys.executable, str(self.cli()), *args],
            capture_output=True,
            text=True,
        )

    def run_verb(
        self, verb: str, *args: str, text: str | None = None
    ) -> tuple[subprocess.CompletedProcess[str], str]:
        """Run ``<verb> <document> [<args>...]`` on a temp document."""
        with tempfile.NamedTemporaryFile("w", suffix=".xml", delete=False) as fh:
            fh.write(self.fixture if text is None else text)
            path = fh.name
        try:
            return self.invoke(verb, path, *args), Path(path).read_text()
        finally:
            Path(path).unlink()

    def test_get_prints_the_version(self) -> None:
        result, _ = self.run_verb("get")
        self.assertEqual(result.stdout.strip(), self.old)

    def test_set_rewrites_exactly_the_version_line(self) -> None:
        result, text = self.run_verb("set", self.new)
        self.assertEqual(result.returncode, 0, result.stderr)
        # Comments and every other byte survive intact.
        self.assertEqual(text, self.fixture.replace(self.old_line, self.new_line))

    def test_set_leaves_no_sidecar_beside_the_document(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            doc = Path(tmp) / "document.xml"
            doc.write_text(self.fixture)
            result = self.invoke("set", str(doc), self.new)
            self.assertEqual(result.returncode, 0, result.stderr)
            # The write lands through a sibling temp + rename; nothing is left.
            self.assertEqual([entry.name for entry in Path(tmp).iterdir()], ["document.xml"])

    def test_set_that_finds_no_line_leaves_the_document_untouched(self) -> None:
        result, after = self.run_verb("set", self.new, text=self.wrong_indent_text)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"no {self.indent}-space", result.stderr)
        self.assertEqual(after, self.wrong_indent_text)

    def test_set_refuses_a_version_that_is_not_element_text(self) -> None:
        # The XML-text rule, both arms: `a<b`, `a&b`, and a control character
        # XML forbids fail to parse; `&#65;` and `<a/>` parse but do not
        # round-trip to the value they were given.
        for bad in ("a<b", "a&b", "\x01", "&#65;", "<a/>"):
            with self.subTest(bad=bad):
                result = self.invoke("set", "document.xml", bad)
                self.assertEqual(result.returncode, 2)
                self.assertIn("that is XML text", result.stderr)

    def test_set_refuses_a_blank_or_whitespace_version(self) -> None:
        # The CLI's own value rule: a version is non-empty and unspaced.
        # argparse rejects both before the document is ever opened, so the
        # path is never read and needs no fixture.
        for bad in ("", "1 0", "1.0 "):
            with self.subTest(bad=bad):
                result = self.invoke("set", "document.xml", bad)
                self.assertEqual(result.returncode, 2)
                self.assertIn("no whitespace", result.stderr)

    def test_a_greater_than_sign_is_legal_element_text(self) -> None:
        # Only `<` and `&` must be escaped in XML text, so `>` lands as the
        # element's text and is not refused.
        result, text = self.run_verb("set", f"{self.new}>b")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(self.new_line.replace(self.new, f"{self.new}>b"), text)

    def test_set_without_a_version_argument_is_a_usage_error(self) -> None:
        # argparse rejects the missing positional before the document is
        # ever read, so the path is never opened and needs no fixture.
        result = self.invoke("set", "document.xml")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)

    def test_a_missing_document_is_a_diagnostic_not_a_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            missing = Path(tmp) / "absent.xml"
            result = self.invoke("get", str(missing))
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{missing}: cannot read", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_get_fails_loudly_on_malformed_xml(self) -> None:
        result, _ = self.run_verb("get", text="not xml at all\n")
        self.assertEqual(result.returncode, 1)
        self.assertIn("not well-formed", result.stderr)

    def test_no_arguments_prints_usage(self) -> None:
        result = self.invoke()
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)

    def test_an_unknown_verb_prints_usage(self) -> None:
        result = self.invoke("delete", "document.xml")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)

    @unittest.skipUnless(
        hasattr(os, "geteuid") and os.geteuid() != 0,
        "a root user writes into a read-only directory",
    )
    def test_an_unwritable_directory_is_a_diagnostic_not_a_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            doc = Path(tmp) / "document.xml"
            doc.write_text(self.fixture)
            # The sidecar temp is created beside the target, so a read-only
            # directory fails the write the way a full disk or a bad mode
            # would — as a diagnostic, not a traceback.
            original = stat.S_IMODE(os.stat(tmp).st_mode)
            os.chmod(tmp, 0o555)
            try:
                result = self.invoke("set", str(doc), self.new)
            finally:
                os.chmod(tmp, original)
            self.assertEqual(doc.read_text(), self.fixture)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{doc}: cannot write", result.stderr)
        self.assertNotIn("Traceback", result.stderr)


# A pom shaped like the real one: default xmlns, comments that must survive
# a set byte-for-byte, and a dependency whose <version> must never be the
# rewrite target.
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


class PomTests(AccessorContract, unittest.TestCase):
    label = "pom.py"
    fixture = POM
    old = "0.1.0"
    new = "0.16.2"
    element = "version"
    indent = 2
    wrong_indent = 4
    refusals = (
        # Blank is as absent as missing — the pom's own policy.
        ("<project><version>  </version></project>\n", ("no project <version>",)),
        ("<project><name>x</name></project>\n", ("no project <version>",)),
        ("<pom><version>0.1.0</version></pom>\n", ("root element is <pom>, not <project>",)),
    )


# A csproj shaped like the real one: an SDK project without a namespace,
# the package <Version> in the one top-level PropertyGroup, comments that
# must survive a set byte-for-byte, and sibling property entries the
# rewrite must leave alone.
CSPROJ = """<?xml version="1.0" encoding="utf-8"?>
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <Version>0.16.1</Version>
    <!-- a comment that ElementTree serialization would drop -->
    <Description>A client.</Description>
  </PropertyGroup>
</Project>
"""


class CsprojTests(AccessorContract, unittest.TestCase):
    label = "csproj.py"
    fixture = CSPROJ
    old = "0.16.1"
    new = "0.16.2"
    element = "Version"
    indent = 4
    wrong_indent = 2
    refusals = (
        # Absent, and a <Version> outside a PropertyGroup is not the
        # package version: direct children of a top-level group only.
        (
            "<Project><PropertyGroup><Description /></PropertyGroup></Project>\n",
            ("no <Version> entry in a PropertyGroup",),
        ),
        (
            "<Project><Version>0.16.1</Version></Project>\n",
            ("no <Version> entry in a PropertyGroup",),
        ),
        # A blank element is its own refusal, not absent: a malformed
        # project never reads as version "".
        (
            "<Project><PropertyGroup><Version>  </Version></PropertyGroup></Project>\n",
            ("the <Version> entry is blank",),
        ),
        # Two entries make "the version" ambiguous — a refusal, not a
        # first-match guess; both values are named.
        (
            "<Project><PropertyGroup><Version>0.16.1</Version></PropertyGroup>"
            "<PropertyGroup><Version>0.15.0</Version></PropertyGroup></Project>\n",
            ("the package version is ambiguous", "0.16.1, 0.15.0"),
        ),
        # Wrong roots: a namespaced <Project> is a namespace refusal, and a
        # wrong element is named by local name however it is spelled.
        ("<Solution><Version>0.16.1</Version></Solution>\n", ("root element is <Solution>, not <Project>",)),
        (
            '<Project xmlns="urn:relate"><PropertyGroup><Version>0.16.1</Version></PropertyGroup></Project>\n',
            ("a namespaced project is not supported",),
        ),
        (
            '<Solution xmlns="urn:relate"><Version>0.16.1</Version></Solution>\n',
            ("root element is <Solution>, not <Project>",),
        ),
    )


if __name__ == "__main__":
    unittest.main()
