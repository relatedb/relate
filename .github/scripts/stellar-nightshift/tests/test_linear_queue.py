"""Tests for linear_queue.py (LABS-311/684) — the Linear-backed
review-task queue: scope policy (explicit Files: section, derived
mentions, one classification channel), cluster shaping and trust, the
durable pull contract, and the backfill curation step. Nightshift's own
plan/window/worker tests live in test_nightshift.py."""

import os
import sys
import unittest

# The ledger fixtures and nightshift's own imports pull pipeline modules
# (facts) from the stellar-review plugin (ADR-0623): STELLAR_ROOT names
# the installed opencode-stellar-review package root. linear_queue and
# nightshift are this repository's, beside these tests.
if not os.environ.get("STELLAR_ROOT"):
    raise unittest.SkipTest(
        "STELLAR_ROOT not set — point it at an installed "
        "opencode-stellar-review package root (ADR-0623)")
sys.path.insert(0, os.path.join(os.environ["STELLAR_ROOT"], "skills", "stellar-review", "tools"))
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import linear_queue  # noqa: E402
import nightshift  # noqa: E402
from helpers import f, led, utc  # noqa: E402


def runnable_cluster():
    """One ledger cluster, as sync/materialize consume: an untrusted
    finding cluster shaped by the same plan the worker runs."""
    return nightshift.plan([led(f("F-1", "relate/relate-executor/src/a.rs",
                                  "blocker"))],
                           set(), utc(3), 25, 8, False)["runnable"][0]


class TestSync(unittest.TestCase):
    def test_linear_is_idempotent(self):
        q = {"runnable": [runnable_cluster()]}
        existing = {}

        def call(query, v):
            # the query constants travel verbatim through the transport —
            # dispatch on exact equality, never substring shapes
            if query == linear_queue.M_CREATE:
                existing["x"] = {"id": "1", "identifier": "LABS-9", "url": "u"}
                return {"issueCreate": {"success": True, "issue": existing["x"]}}
            assert query == linear_queue.Q_FIND, query
            return {"issues": {"nodes": [existing["x"]] if "x" in existing else []}}

        first = linear_queue.materialize(q, "team", "LBL", call)
        second = linear_queue.materialize(q, "team", "LBL", call)
        self.assertTrue(first[0]["created"])
        self.assertFalse(second[0]["created"])

    def test_issue_carries_review_tasks_label(self):
        self.assertEqual(
            linear_queue.issue_input(runnable_cluster(), "team", "LBL")["labelIds"],
            ["LBL"])

    def test_resolve_creates_missing_label(self):
        def call(q, v):
            if q == linear_queue.Q_TEAM:
                return {"teams": {"nodes": [{"id": "T"}]}}
            if q == linear_queue.Q_LABEL:
                return {"issueLabels": {"nodes": []}}
            assert q == linear_queue.M_LABEL, q
            return {"issueLabelCreate": {"issueLabel": {"id": "NEW"}}}
        self.assertEqual(linear_queue.resolve("LABS", "review-tasks", call), ("T", "NEW"))


class TestScopePolicy(unittest.TestCase):
    DESC = ("Fix these.\n\n**Files:**\n- `relate/relate-planner/src/rewrite.rs`\n"
            "- surfaces/relate-python/src/convert.rs\n- ../evil\n\nOther text relate/x/y.rs")

    def task(self, desc, key=None):
        return {"identifier": "LABS-295", "title": "t", "description": desc, "key": key}

    def test_parse_files_section_only(self):
        self.assertEqual(linear_queue.parse_files(self.DESC),
                         ["relate/relate-planner/src/rewrite.rs",
                          "surfaces/relate-python/src/convert.rs"])
        self.assertEqual(linear_queue.parse_files("no section relate/a/b.rs"), [])

    def test_parse_files_unescapes_linear_markdown(self):
        d = "**Files:**\n- `relate/relate-executor/src/exec\\_ops.rs`\n"
        self.assertEqual(linear_queue.parse_files(d),
                         ["relate/relate-executor/src/exec_ops.rs"])

    def test_scan_reads_in_situ_mentions(self):
        # the fallback scope's mechanism: every workspace path the text
        # itself mentions, through the one pipeline
        d = ("Fix `surfaces/relate-cli/src/serve\\_args.rs:13` and the "
             "tests in surfaces/relate-cli/src/serve_cmd/tests.rs; "
             "see docs/adr/0103.md and ../evil")
        self.assertEqual(linear_queue._scan_paths(d),
                         ["surfaces/relate-cli/src/serve_args.rs",
                          "surfaces/relate-cli/src/serve_cmd/tests.rs"])

    def test_scan_strips_urls_whole(self):
        # `.../relate/pull/530` and its siblings match the
        # workspace-path shape; a URL is stripped whole, by construction
        # — host-shaped tokens of ANY host, no enumeration to drift
        # (observed on the real backlog: LABS-621/554/430)
        d = ("Follow-up of https://github.com/relatedb/relate/pull/530, "
             "github.com/relatedb/relate/issues/684, "
             "linear.app/relatedb/relate/pull/530 and "
             "raw.githubusercontent.com/relatedb/relate/main/x.rs.")
        self.assertEqual(linear_queue._scan_paths(d), [])

    def test_scan_trims_sentence_punctuation(self):
        # an unquoted sentence-final mention carries its period into the
        # PATH_RE match; no workspace path ends in sentence punctuation
        d = "Fix surfaces/relate-cli/src/main.rs."
        self.assertEqual(linear_queue._scan_paths(d),
                         ["surfaces/relate-cli/src/main.rs"])

    def test_task_clusters_derives_when_no_section(self):
        # a missing Files: section is a fallback case, not a skip: the
        # items' own path mentions shape the same clusters — but
        # UNTRUSTED (round 3, the security boundary): derived prose is
        # attacker-influenceable until curated, and backfill's --apply
        # is the curation step
        t = self.task("Fix `relate/relate-planner/src/rewrite.rs` per F-3-2.")
        cs = linear_queue.task_clusters(t)
        self.assertEqual(t["scope"], "derived")
        self.assertEqual([c["crate"] for c in cs], ["relate/relate-planner"])
        self.assertFalse(cs[0]["trusted"])

    def test_task_clusters_skips_only_when_no_path_at_all(self):
        t = self.task("Improve the docs navigation; see the website mockups.")
        self.assertEqual(linear_queue.task_clusters(t), [])
        self.assertEqual(t["scope"], "none")
        self.assertIn("no derivable path", linear_queue.SKIP_REASONS["none"])

    def test_task_cluster_skips(self):
        # only a ticket naming no workspace path at all skips; the
        # reason prose is composed from the one scope vocabulary
        t = self.task("nothing")
        self.assertEqual(linear_queue.task_clusters(t), [])
        self.assertEqual(t["scope"], "none")
        self.assertEqual(linear_queue.SKIP_REASONS["none"],
                         "no Files: section and no derivable path")
        self.assertEqual(linear_queue.task_clusters(self.task(self.DESC, key="abc")), [])

    def test_empty_section_is_explicit_not_derived(self):
        # a PRESENT section naming no workspace path is the human's
        # explicit answer — never overridden by prose mentions, and
        # backfill never appends a second section
        t = self.task("Files:\n- docs/adr/0103.md\n\n"
                      "Also see relate/relate-core/src/lib.rs.")
        self.assertEqual(linear_queue.task_clusters(t), [])
        self.assertEqual(t["scope"], "empty-section")
        self.assertEqual(linear_queue.SKIP_REASONS["empty-section"],
                         "Files: section names no workspace path")

    def test_task_clusters_shaped_per_crate(self):
        # a task is an input stream: its scope is shaped into one small
        # per-crate cluster (the 5-file/4-crate "P3 on merged main"
        # issue rode one session before this and failed its tier)
        t = self.task(self.DESC)
        cs = linear_queue.task_clusters(t)
        self.assertEqual(t["scope"], "explicit")
        crates = sorted(c["crate"] for c in cs)
        self.assertEqual(crates, ["relate/relate-planner",
                                  "surfaces/relate-python"])
        for c in cs:
            self.assertLessEqual(len(c["files"]), linear_queue.FILES_PER_CLUSTER)
            self.assertTrue(c["trusted"])
            self.assertTrue(c["id"].startswith("TASK-LABS-295-"))
        # whole-stream facts, outside the per-cluster loop: keys are
        # unique and stable (re-runs dedupe, never duplicate)
        again = linear_queue.task_clusters(self.task(self.DESC))
        self.assertEqual({x["key"] for x in cs}, {x["key"] for x in again})
        self.assertEqual(len({x["key"] for x in cs}), len(cs))

    def test_ledger_clusters_untrusted_only_explicit_tasks_trusted(self):
        # the trust boundary (round 3, the security review): ledger
        # clusters and DERIVED task clusters are untrusted — only a
        # curated Files: section reaches the write-privileged worker
        self.assertFalse(runnable_cluster()["trusted"])
        explicit = linear_queue.task_clusters(self.task(self.DESC))
        self.assertTrue(all(c["trusted"] for c in explicit))
        derived = linear_queue.task_clusters(self.task(
            "Fix `relate/relate-core/src/lib.rs` per F-3-2."))
        self.assertTrue(derived)
        self.assertFalse(any(c["trusted"] for c in derived))


class TestPull(unittest.TestCase):
    def test_parses_marker_and_keeps_hand_filed(self):
        c = runnable_cluster()
        nodes = [{"id": "1", "identifier": "LABS-1", "url": "u", "description": "",
                  "title": f"{linear_queue.marker(c)} x"},
                 # marker-free WITH a Files: section — the non-vacuous
                 # arm: this node must actually shape clusters
                 {"id": "2", "identifier": "LABS-2", "url": "u",
                  "description": "Files:\n- relate/relate-core/src/lib.rs\n",
                  "title": "hand filed"}]
        got = linear_queue.pull("review-tasks",
                                lambda q, v: {"issues": {"nodes": nodes}})
        # pull returns the durable contract — marker key AND shaped
        # clusters, on a fixture where the shapeable arm really runs —
        # with the explicit-scope ticket fetched first (LABS-684)
        self.assertEqual([g["key"] for g in got], [None, c["key"]])
        shaped = [t for t in got if not t["skipped"]]
        self.assertEqual([t["identifier"] for t in shaped], ["LABS-2"])
        self.assertTrue(shaped[0]["clusters"])

    def test_sorts_explicit_scope_first(self):
        derived = {"id": "1", "identifier": "LABS-1", "url": "u", "title": "t",
                   "description": "Fix `relate/relate-core/src/lib.rs`."}
        explicit = {"id": "2", "identifier": "LABS-2", "url": "u", "title": "t",
                    "description": "Files:\n- surfaces/relate-cli/src/main.rs\n"}
        # API order puts the derived-scope ticket first; pull must invert
        got = linear_queue.pull("review-tasks",
                                lambda q, v: {"issues": {"nodes": [derived, explicit]}})
        self.assertEqual([t["identifier"] for t in got], ["LABS-2", "LABS-1"])


class TestBackfill(unittest.TestCase):
    NODES = [
        {"id": "1", "identifier": "LABS-1", "url": "u", "title": "t",
         "description": "Files:\n- surfaces/relate-cli/src/main.rs\n"},
        {"id": "2", "identifier": "LABS-2", "url": "u", "title": "t",
         "description": "Fix `relate/relate-core/src/lib.rs` per F-3-2."},
        {"id": "3", "identifier": "LABS-3", "url": "u", "title": "t",
         "description": "Rewrite the onboarding prose."},
        {"id": "4", "identifier": "LABS-4", "url": "u",
         "title": "[stellar-ns:0123456789] stellar findings in relate-core",
         "description": "cluster body"},
    ]

    def test_dry_run_reports_and_apply_writes_once(self):
        # the query constants travel verbatim through the transport —
        # the stub dispatches on exact equality, not substring shapes
        updates = []

        def call(q, v):
            if q == linear_queue.Q_PULL:
                return {"issues": {"nodes": self.NODES}}
            assert q == linear_queue.M_UPDATE, q
            updates.append(v)
            return {"issueUpdate": {"success": True}}

        report = linear_queue.backfill("review-tasks", call, apply=False)
        by = {r["identifier"]: r for r in report}
        self.assertEqual(by["LABS-1"]["action"], "has-section")
        self.assertEqual(by["LABS-1"]["scope"], "explicit")
        self.assertEqual(by["LABS-2"]["action"], "would-append")
        self.assertEqual(by["LABS-2"]["files"], ["relate/relate-core/src/lib.rs"])
        self.assertEqual(by["LABS-2"]["scope"], "derived")
        self.assertEqual(by["LABS-3"]["action"], "no-derivable-path")
        self.assertEqual(by["LABS-4"]["action"], "skip-auto-filed")
        self.assertEqual(updates, [])

        report = linear_queue.backfill("review-tasks", call, apply=True)
        self.assertEqual([r["action"] for r in report if r["identifier"] == "LABS-2"],
                         ["appended"])
        # exactly one write, appending a section parse_files can read
        # back (the round-trip contract), and never to a ticket that
        # already carries one
        self.assertEqual(len(updates), 1)
        self.assertEqual(linear_queue.parse_files(updates[0]["i"]["description"]),
                         ["relate/relate-core/src/lib.rs"])
