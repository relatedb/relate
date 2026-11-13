"""Tests for scripts/front_bench_corpus.py — the front-bench statement
corpus builder.

Covers the Gherkin walk's one-sink container model (a step before any
Background/Scenario is a loud parse error, never a silent drop; a second
Background reopens the background sink), the doc-string dedent through
`textwrap.dedent` — including the whitespace-only-line normalization a
hand-rolled margin walk differs on — the top-level-semicolon splitter's
quote tracking, the keyword filter, and `main`'s one dedupe pass, where
a statement in both sources is attributed to the workload list that
contributed it first.
"""

import contextlib
import io
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from script_loader import ROOT, load

SUBJECT = load("front_bench_corpus", ROOT / "scripts" / "front_bench_corpus.py")


def feature_text(*lines: str) -> str:
    return "\n".join(lines) + "\n"


def tck_statements_of(*lines: str) -> list[str]:
    """The statements `tck_statements` reads out of one synthetic
    `.feature` file — the setup every tck_statements case shares."""
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "a.feature").write_text(feature_text(*lines), encoding="utf-8")
        return SUBJECT.tck_statements(root)


class ParseFeatureTests(unittest.TestCase):
    """The Gherkin walk: one sink routes each step to its container."""

    def test_background_and_scenario_steps_reach_their_containers(self) -> None:
        feature = SUBJECT.parse_feature(
            feature_text(
                "Feature: F",
                "",
                "  Background:",
                "    Given an empty graph",
                "",
                "  Scenario: S",
                "    When executing query:",
                '      """',
                "      MATCH (n)",
                "      RETURN n",
                '      """',
            )
        )
        self.assertEqual([s.doc_string for s in feature.background], [None])
        self.assertEqual(len(feature.scenarios), 1)
        self.assertEqual(
            [s.doc_string for s in feature.scenarios[0].steps],
            ["MATCH (n)\nRETURN n"],
        )

    def test_doc_string_whitespace_only_lines_normalize_to_empty(self) -> None:
        # The dedent is `textwrap.dedent`, and its whitespace-only-line
        # behavior is load-bearing: a hand-rolled margin walk left a
        # whitespace-only line longer than the common indent with its excess
        # spaces, where textwrap.dedent empties it. The corpus must carry
        # the normalized form.
        feature = SUBJECT.parse_feature(
            feature_text(
                "  Scenario: S",
                "    When executing query:",
                '      """',
                "      MATCH (n)",
                "        ",
                "      RETURN n",
                '      """',
            )
        )
        self.assertEqual(
            [s.doc_string for s in feature.scenarios[0].steps],
            ["MATCH (n)\n\nRETURN n"],
        )

    def test_a_step_before_any_container_is_a_loud_parse_error(self) -> None:
        # A step outside any Background/Scenario is not valid Gherkin: the
        # walk names the file and exits rather than dropping the step.
        with self.assertRaises(SystemExit) as caught:
            SUBJECT.parse_feature(
                feature_text(
                    "Feature: F",
                    "    Given a stray step",
                    "  Scenario: S",
                    "    Then ok",
                ),
                source="stray.feature",
            )
        self.assertIn("stray.feature", str(caught.exception))
        self.assertIn("outside any Background/Scenario", str(caught.exception))

    def test_a_second_background_reopens_the_background_sink(self) -> None:
        feature = SUBJECT.parse_feature(
            feature_text(
                "  Background:",
                "    Given a:",
                '      """',
                "      MATCH (a)",
                '      """',
                "  Scenario: S",
                "    Then b:",
                '      """',
                "      MATCH (b)",
                '      """',
                "  Background:",
                "    Given c:",
                '      """',
                "      MATCH (c)",
                '      """',
            )
        )
        self.assertEqual(
            [s.doc_string for s in feature.background], ["MATCH (a)", "MATCH (c)"]
        )
        self.assertEqual(
            [s.doc_string for s in feature.scenarios[0].steps], ["MATCH (b)"]
        )

    def test_scenario_outline_opens_a_scenario_and_examples_are_walked_past(
        self,
    ) -> None:
        feature = SUBJECT.parse_feature(
            feature_text(
                "  Scenario Outline: O",
                "    Then ok",
                "    Examples:",
                "      | x |",
                "      | 1 |",
            )
        )
        self.assertEqual(len(feature.scenarios), 1)
        self.assertEqual(len(feature.scenarios[0].steps), 1)

    def test_step_table_is_walked_past_without_becoming_a_step(self) -> None:
        feature = SUBJECT.parse_feature(
            feature_text(
                "  Scenario: S",
                "    Then the result should be:",
                "      | n |",
                "      | 1 |",
                "    And a second step:",
                '      """',
                "      MATCH (n)",
                '      """',
            )
        )
        self.assertEqual(
            [s.doc_string for s in feature.scenarios[0].steps],
            [None, "MATCH (n)"],
        )

    def test_doc_string_followed_by_a_table_keeps_the_doc_string(self) -> None:
        feature = SUBJECT.parse_feature(
            feature_text(
                "  Scenario: S",
                "    Then ok:",
                '      """',
                "      MATCH (n)",
                '      """',
                "      | n |",
                "      | 1 |",
            )
        )
        self.assertEqual(
            [s.doc_string for s in feature.scenarios[0].steps], ["MATCH (n)"]
        )

    def test_comments_and_blank_lines_are_skipped(self) -> None:
        feature = SUBJECT.parse_feature(
            feature_text(
                "# a comment",
                "",
                "  Scenario: S",
                "    # another",
                "    Then ok",
            )
        )
        self.assertEqual(len(feature.scenarios[0].steps), 1)


class SplitStatementsTests(unittest.TestCase):
    """The bench splitter: top-level semicolons only, quotes tracked."""

    def test_top_level_semicolons_split_and_empty_pieces_drop(self) -> None:
        self.assertEqual(
            SUBJECT.split_statements("MATCH (n) RETURN n;\n\n; RETURN 1;"),
            ["MATCH (n) RETURN n", "RETURN 1"],
        )

    def test_semicolon_inside_a_string_is_not_a_boundary(self) -> None:
        self.assertEqual(
            SUBJECT.split_statements("RETURN 'a;b'; RETURN 2"),
            ["RETURN 'a;b'", "RETURN 2"],
        )

    def test_the_other_quote_kind_inside_a_string_is_ordinary(self) -> None:
        self.assertEqual(
            SUBJECT.split_statements("RETURN 'it\"s; fine'"),
            ['RETURN \'it"s; fine\''],
        )

    def test_final_statement_without_a_semicolon_is_kept(self) -> None:
        self.assertEqual(SUBJECT.split_statements("RETURN 1"), ["RETURN 1"])

    def test_blank_input_yields_no_statements(self) -> None:
        self.assertEqual(SUBJECT.split_statements("  ;\n;  "), [])


class ReadStrictTests(unittest.TestCase):
    def test_a_non_utf8_source_fails_loud_and_names_the_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bad.cypher"
            path.write_bytes(b"MATCH \xff")
            with self.assertRaises(SystemExit) as caught:
                SUBJECT.read_strict(path)
        self.assertIn(str(path), str(caught.exception))


class WorkloadStatementsTests(unittest.TestCase):
    def test_every_statement_of_every_workload_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            workloads = root / "testing/relate-query-latency/workloads"
            workloads.mkdir(parents=True)
            (workloads / "indexes.cypher").write_text(
                "CREATE INDEX a FOR (n:L) ON (n.p);\n"
                "CREATE INDEX b FOR (n:L) ON (n.q);",
                encoding="utf-8",
            )
            (workloads / "one.cypher").write_text("MATCH (n) RETURN n", encoding="utf-8")
            with mock.patch.object(SUBJECT, "REPO", root):
                statements = SUBJECT.workload_statements()
        self.assertEqual(
            statements,
            [
                "CREATE INDEX a FOR (n:L) ON (n.p)",
                "CREATE INDEX b FOR (n:L) ON (n.q)",
                "MATCH (n) RETURN n",
            ],
        )


class TckRootTests(unittest.TestCase):
    """$TCK_CORPUS resolution: unset is a skip, a named shelf must hold a
    `.feature` file or the run exits loud."""

    def setUp(self) -> None:
        self.prior = os.environ.pop(SUBJECT.TCK_ENV, None)

    def tearDown(self) -> None:
        os.environ.pop(SUBJECT.TCK_ENV, None)
        if self.prior is not None:
            os.environ[SUBJECT.TCK_ENV] = self.prior

    def test_unset_is_none(self) -> None:
        self.assertIsNone(SUBJECT.tck_root())

    def test_empty_is_none(self) -> None:
        os.environ[SUBJECT.TCK_ENV] = ""
        self.assertIsNone(SUBJECT.tck_root())

    def test_a_named_non_directory_fails_loud(self) -> None:
        os.environ[SUBJECT.TCK_ENV] = "/nonexistent/tck-shelf"
        with self.assertRaises(SystemExit) as caught:
            SUBJECT.tck_root()
        self.assertIn("not a directory", str(caught.exception))

    def test_a_directory_without_a_feature_file_fails_loud(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            os.environ[SUBJECT.TCK_ENV] = tmp
            with self.assertRaises(SystemExit) as caught:
                SUBJECT.tck_root()
        self.assertIn("no .feature file", str(caught.exception))

    def test_a_shelf_with_a_feature_file_is_returned(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "a.feature").write_text("Feature: F\n", encoding="utf-8")
            os.environ[SUBJECT.TCK_ENV] = tmp
            self.assertEqual(SUBJECT.tck_root(), Path(tmp))


class TckStatementsTests(unittest.TestCase):
    def test_only_keyword_led_doc_strings_are_kept(self) -> None:
        self.assertEqual(
            tck_statements_of(
                "Feature: F",
                "  Scenario: S",
                "    When executing query:",
                '      """',
                "      MATCH (n)",
                "      RETURN n",
                '      """',
                "    Then the result should be:",
                '      """',
                "      | n |",
                '      """',
            ),
            ["MATCH (n)\nRETURN n"],
        )

    def test_the_keyword_match_is_case_insensitive(self) -> None:
        self.assertEqual(
            tck_statements_of(
                "Feature: F",
                "  Scenario: S",
                "    When executing query:",
                '      """',
                "      match (n) return n",
                '      """',
            ),
            ["match (n) return n"],
        )


class MainTests(unittest.TestCase):
    """main() over stubbed collectors — the dedupe pass, its workload-first
    attribution, and the summary it prints."""

    def run_main(
        self, workloads: list[str], tck: list[str] | None
    ) -> tuple[str, str, Path]:
        """Run main() with stubbed collectors; return (stdout, corpus text,
        the corpus path main wrote)."""
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "corpus.txt"
            buffer = io.StringIO()
            # One corpus value: the tck_root stub returns the temp dir only
            # when the corpus is set (never a path the collector ignores),
            # and the tck_statements stub returns the statements — so
            # "unset" is tested once. When unset, main() takes its skip
            # branch and never calls the collector, so the stub needs no
            # fallback: a contract change surfaces as a loud TypeError.
            root = Path(tmp) if tck is not None else None
            with mock.patch.multiple(
                SUBJECT,
                workload_statements=lambda: list(workloads),
                tck_root=lambda: root,
                tck_statements=lambda _root: list(tck),
                OUT=out,
            ):
                with contextlib.redirect_stdout(buffer):
                    SUBJECT.main()
                text = out.read_text(encoding="utf-8")
            return buffer.getvalue(), text, out

    def test_dedupe_is_workload_first_and_credits_the_contributor(self) -> None:
        stdout, corpus, out = self.run_main(["A", "B", "C"], ["B", "D"])
        self.assertEqual(corpus, "A\x1eB\x1eC\x1eD")
        self.assertIn("workloads: 3  tck: 1  total: 4", stdout)
        self.assertIn("total bytes: 4", stdout)
        self.assertIn(f"wrote {out}", stdout)

    def test_unset_corpus_prints_the_skip_and_writes_workloads_only(self) -> None:
        stdout, corpus, _ = self.run_main(["A"], None)
        self.assertEqual(corpus, "A")
        self.assertIn("tck: skipped", stdout)
        self.assertIn("workloads: 1  tck: 0  total: 1", stdout)


if __name__ == "__main__":
    unittest.main()
