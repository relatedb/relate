import unittest

from script_loader import ROOT, load

residuals = load("rename_residuals", ROOT / "scripts" / "rename-residuals.py")


class ClassifyTests(unittest.TestCase):
    """One keep class per rule, then the strays the rename must catch."""

    def test_on_disk_artifact_names_are_keeps(self) -> None:
        for line in [
            "takes the `.cypherlite.lock` and an exclusive `.cypherlite.writer` lease",
            "the `.cypherlite/schema/schema.cypher` mirror (ADR-0481)",
            'const ROLE: &str = "CYPHERLITE-LOCK-v1";',
        ]:
            self.assertEqual(residuals.classify("relate/relate-storage/src/x.rs", line), "on-disk artifact")

    def test_guard_and_notice_and_precutover_lines_are_keeps(self) -> None:
        cases = [
            ("relate/relate/src/open_config.rs", 'if !upper.starts_with("CYPHERLITE_") {', "rename guard"),
            ("surfaces/relate-cli/tests/e2e_version.rs", "cYpHeRlItE_pReViEw", "rename guard"),
            ("surfaces/relate-cli/src/upgrade_cmd/mod.rs", "cypherlite is now relate — this is the final", "swan-song notice"),
            (".github/workflows/spec-harness.yml", "repository: neo4j-labs/cypherlite-cypher-test-harness", "pre-cutover name"),
            ("docs/guides/x.md", 'doc": "adr/0046-cypherlite-parser-memorydb-runtime.md', "ADR filename ref"),
        ]
        for path, line, want in cases:
            self.assertEqual(residuals.classify(path, line), want, f"{path}: {line}")

    def test_old_env_names_outside_the_guard_files_are_strays(self) -> None:
        for path in ["relate/relate/src/open.rs", "docs/api/cli/spec.md", "scripts/check-os-matrix.py"]:
            self.assertEqual(residuals.classify(path, "reads CYPHERLITE_PREVIEW at open"), "stray")

    def test_brand_prose_is_a_stray_even_in_guard_files(self) -> None:
        self.assertEqual(
            residuals.classify("relate/relate/src/open_config.rs", "CypherLite is the embedded engine"),
            "stray",
        )

    def test_lines_without_the_old_name_are_none(self) -> None:
        self.assertIsNone(residuals.classify("docs/api/cli/spec.md", "the relate binary"))


class RepoScanTests(unittest.TestCase):
    """The gate itself: the tracked tree carries no strays."""

    def test_scan_reports_no_strays(self) -> None:
        strays = [e for e in residuals.scan(ROOT) if e[3] == "stray"]
        self.assertEqual(strays, [])

    def test_scan_classifies_the_known_keeps(self) -> None:
        classes = {e[3] for e in residuals.scan(ROOT)}
        self.assertIn("frozen record", classes)
        self.assertIn("on-disk artifact", classes)
        self.assertIn("rename guard", classes)


if __name__ == "__main__":
    unittest.main()
