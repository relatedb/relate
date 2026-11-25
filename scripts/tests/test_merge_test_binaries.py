"""Unit-test the integration-test-binary merge (scripts/merge-test-binaries.py).

The policies this suite guards, one class each: the scan's rules (a rule named
in prose does not keep a file, a real call site always does); the generated
`main.rs` — the committed copies must stay byte-equal to what `render_main`
produces, since the script regenerates them and nothing else type-checks the
emitted Rust; the layout (every file left under `tests/` is one the scan would
keep, and no file inside `tests/<binary>/` is one it would keep); the
reference rewrite (no tracked file still names a merged stem at its pre-move
path); and the recorded keeps (each exists and names its reason).
"""


import unittest


from script_loader import ROOT, load

merge = load("merge_test_binaries", ROOT / "scripts" / "merge-test-binaries.py")

BINARY = merge.DEFAULT_BINARY

# The crates that keep their shareable test files in one binary, derived from
# the tree so a newly merged crate is covered the day its directory lands.
# The glob is fixed-depth (`<group>/<crate>/tests/<binary>/main.rs` — every
# workspace member sits at `<group>/<crate>/`), so a build tree cannot match
# it: its paths are deeper and differently shaped.


def crates() -> list[str]:
    return sorted(
        main.parent.parent.parent.relative_to(ROOT).as_posix()
        for main in ROOT.glob(f"*/*/tests/{BINARY}/main.rs")
    )


def layout(crate: str) -> merge.Plan:
    """The plan the script itself would execute for one crate — the one
    spelling of the layout path (`layout.target`) inside the guard too."""
    return merge.plan(ROOT / crate)


def committed_main(crate: str) -> tuple[str, str]:
    """(render_main output, committed text) for one crate's main.rs, both
    from the same plan the script itself would execute."""
    plan = layout(crate)
    rendered = merge.render_main(modules=plan.modules, shared=plan.shared)
    return rendered, (plan.target / "main.rs").read_text()


class MainDriftTests(unittest.TestCase):
    """The committed main.rs files are the generator's output, byte for byte."""

    def test_at_least_one_crate_is_merged(self) -> None:
        self.assertTrue(crates(), "no tests/integration/main.rs found to check")

    def test_committed_main_matches_the_generator(self) -> None:
        for crate in crates():
            with self.subTest(crate=crate):
                rendered, committed = committed_main(crate)
                self.assertEqual(rendered, committed)


class LayoutTests(unittest.TestCase):
    """The scan agrees with the committed layout, in both directions."""

    def test_no_file_left_at_the_top_level_would_move(self) -> None:
        for crate in crates():
            with self.subTest(crate=crate):
                self.assertEqual(
                    [path.name for path in layout(crate).moved],
                    [],
                    f"{crate}: a bare re-run would move these into tests/{BINARY}/",
                )

    def test_no_merged_file_would_be_kept_separate(self) -> None:
        # The invariant the preview-opt-in race needed: a file that only
        # declares its dependence in prose must not be merged, or its
        # process-global `Once` breaks a closed-gate sibling.
        for crate in crates():
            plan = layout(crate)
            for stem in plan.existing:
                with self.subTest(crate=crate, file=stem):
                    self.assertIsNone(
                        merge.isolation_reason((plan.target / f"{stem}.rs").read_text()),
                        f"{crate}/tests/{BINARY}/{stem}.rs would be kept separate",
                    )


class ReferenceRewriteTests(unittest.TestCase):
    """The reference rewrite left no mention behind — through the producer's
    own mention scan, not a re-derivation."""

    def test_the_substitution_rewrites_a_repo_relative_mention(self) -> None:
        # The positive control for the scan's own spelling: the prefix must
        # be repo-relative (an absolute one matches nothing in `git grep`'s
        # repo-relative output, and both the rewrite and the invariant below
        # would pass vacuously).
        crate = "relate/relate"
        scan = merge.mention_scan(ROOT / crate, ROOT, ["clone"])
        # Assembled, not literal: the invariant below scans tracked files for
        # the pre-move spelling, and this test is itself a tracked file.
        mention = f"see {crate}/tests/clone.rs for the clone suite\n"
        self.assertEqual(
            scan.substitution.sub(
                lambda m: (scan.target / f"{m.group(1)}.rs").as_posix(), mention
            ),
            f"see {crate}/tests/{merge.DEFAULT_BINARY}/clone.rs for the clone suite\n",
        )

    def test_no_tracked_file_mentions_a_merged_stem_at_its_pre_move_path(self) -> None:
        # A rebuild once rewrote nothing in silence (the listing grep carried
        # `\b`, which git's ERE lacks) and every doc kept the pre-move path.
        # The guard asks the producer's own scan, so whatever goes wrong with
        # the pattern, the exclusion, or the grep, a stale mention reds here.
        for crate in crates():
            with self.subTest(crate=crate):
                stems = layout(crate).modules
                self.assertTrue(stems, f"{crate}: no modules to scan")
                scan = merge.mention_scan(ROOT / crate, ROOT, stems)
                self.assertEqual(
                    [path.relative_to(ROOT).as_posix() for path in scan.files],
                    [],
                    f"{crate}: stale pre-move mentions",
                )


class ScanRuleTests(unittest.TestCase):
    """The rules that keep a file its own binary, and their two edges."""

    def test_a_rule_in_a_doc_comment_does_not_keep_a_file(self) -> None:
        text = "/// what runs, so `current_exe()` resolves to it.\nfn f() {}\n"
        self.assertIsNone(merge.isolation_reason(text))

    def test_a_real_call_site_keeps_a_file(self) -> None:
        self.assertEqual(
            merge.isolation_reason("fn f() { current_exe(); }\n"),
            "it re-executes its own binary",
        )
        self.assertEqual(
            merge.isolation_reason("fn f() { std::env::set_var(\"A\", \"B\"); }\n"),
            "it mutates the process environment",
        )

    def test_a_url_in_a_string_is_not_a_comment(self) -> None:
        # A `//` inside a string literal must not truncate the line: the
        # real call after it still keeps the file.
        text = 'fn f() { let u = "http://x"; std::env::set_var("A", "B"); }\n'
        self.assertEqual(merge.isolation_reason(text), "it mutates the process environment")

    def test_a_trailing_comment_is_ignored(self) -> None:
        self.assertIsNone(merge.isolation_reason("fn f() {} // current_exe()\n"))

    def test_the_closed_gate_keeps_a_file(self) -> None:
        self.assertIsNotNone(merge.isolation_reason("fn f() { assert_preview_unset(); }\n"))

    def test_the_preview_rule_reads_comments(self) -> None:
        # The knob is process-global and authors declare dependence in
        # prose; a comment mention must keep the file, unlike the other
        # rules (a doc comment naming `current_exe()` does not).
        self.assertIsNotNone(
            merge.isolation_reason("//! `RELATE_PREVIEW` is process-global.\nfn f() {}\n")
        )
        self.assertIsNotNone(
            merge.isolation_reason("fn f() { show_variants_connection(); }\n")
        )


class RenderMainTests(unittest.TestCase):
    """The generated main.rs declares the shared module and the guard."""

    def test_the_shared_module_is_declared_once_with_the_path(self) -> None:
        # No `#[allow(dead_code)]` here: the shared module carries its own
        # inner allow (each including binary uses a different subset of its
        # helpers), and emitting one too is a duplicated attribute under
        # clippy's `-D warnings`.
        rendered = merge.render_main(modules=[], shared=["common"])
        self.assertIn('#[path = "../common/mod.rs"]\nmod common;\n', rendered)
        self.assertNotIn("allow(dead_code)", rendered)

    def test_the_guard_calls_the_one_helper(self) -> None:
        rendered = merge.render_main(modules=[], shared=[])
        self.assertIn(
            "relate_test_support::test_binaries::assert_every_file_is_declared(", rendered
        )


class KeepAlwaysTests(unittest.TestCase):
    """The recorded keeps exist, and every recorded crate is merged."""

    def test_every_keep_always_crate_is_merged(self) -> None:
        for crate in merge.KEEP_ALWAYS:
            with self.subTest(crate=crate):
                self.assertIn(crate, crates())

    def test_every_recorded_keep_names_its_reason(self) -> None:
        for crate, keeps in merge.KEEP_ALWAYS.items():
            for stem, reason in keeps.items():
                with self.subTest(crate=crate, stem=stem):
                    self.assertTrue(
                        reason.strip(),
                        f"{crate}: the recorded keep {stem} has no reason",
                    )

    def test_recorded_keeps_still_exist(self) -> None:
        for crate, keeps in merge.KEEP_ALWAYS.items():
            for stem in keeps:
                with self.subTest(crate=crate, stem=stem):
                    self.assertTrue(
                        (layout(crate).tests_dir / f"{stem}.rs").is_file(),
                        f"{crate}/tests/{stem}.rs is recorded but gone",
                    )



if __name__ == "__main__":
    unittest.main()
