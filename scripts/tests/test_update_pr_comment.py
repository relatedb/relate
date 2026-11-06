"""Tests for update-pr-comment.sh — the self-updating loadtest PR comment.

`gh` is stubbed on PATH (scripts/tests/gh_stub.py behind a /bin/sh shim;
it never touches the network). The stub records every `gh api` call in
$GH_STUB_STATE/calls.log — endpoint, method, --jq filter, --paginate
flag, and the exact request body for writes — and serves comments with
authors, attributing POSTs to github-actions[bot] exactly as the real
API attributes a GITHUB_TOKEN comment. That evolving state proves two
properties against API-like state: a rerun PATCHes the comment it
created instead of duplicating, and the author filter selects only the
bot's marker comment — a third-party comment that quotes the marker is
never edited, and never suppresses the fresh bot comment.

The comment's overall status line is read from the document's
producer-owned `**Status:** ok|warning` line (never from the
`**Pairs:**` tokens); a missing or unrecognized status line falls back
to warning — fail loud in presentation, never silently healthy. The
RoundTripTests run the REAL aggregator (compare-baselines.sh
--render-only over the REAL comparator) and feed its document to the
REAL updater, so the comparator → aggregator → updater token chain
cannot drift silently.
"""

import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from loadtest_fixtures import report


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "update-pr-comment.sh"
AGGREGATOR = ROOT / "scripts" / "compare-baselines.sh"
GH_STUB = ROOT / "scripts" / "tests" / "gh_stub.py"
MARKER = "<!-- relate:loadtest-baselines -->"
BOT = "github-actions[bot]"
LIST_FILTER = (
    f'.[] | select(.user.login == "{BOT}") '
    f'| select((.body // "") | contains("{MARKER}")) | .id'
)

BASH = shutil.which("bash")

REPO = "octo-labs/relate"
RUN_URL = f"https://github.example/{REPO}/actions/runs/987654321"
SHA = "1234abcddeadbeefcafe"

BASE_ENV = {
    "GITHUB_REPOSITORY": REPO,
    "GITHUB_SERVER_URL": "https://github.example",
    "GITHUB_RUN_ID": "987654321",
    "GITHUB_SHA": SHA,
    "GH_TOKEN": "stub-token",
}

# Never inherit runner/dev-machine GitHub state: the updater must be
# deterministic under the test's own environment.
POPPED = (
    "PR_NUMBER",
    "COMPARISON_OUT",
    "REPORTS_DIR",
    "GITHUB_STEP_SUMMARY",
    "GITHUB_ACTIONS",
    "GITHUB_API_URL",
    "GITHUB_TOKEN",
    "GH_HOST",
    "GH_REPO",
    "GITHUB_ENTERPRISE_URL",
    "GH_ENTERPRISE_TOKEN",
    "GH_STUB_MODE",
)

# External commands update-pr-comment.sh uses (everything else is a
# builtin). Symlinked into a sandbox bin so the child PATH is hermetic —
# a real `gh` can never leak into a test.
COREUTILS = ("cat", "dirname", "grep", "mktemp", "rm")

# Canned documents mirror the aggregator's shape: the producer-owned
# `**Status:**` line sits on its own line, after the intro, immediately
# before the `**Pairs:**` summary.
CLEAN_DOC = """# Loadtest baseline comparison

Frozen baselines vs current reports; bands match the `compare-loadtests` gate.

**Status:** ok

**Pairs:** fraud-loadtest: compared · identity-loadtest: compared · fraud-checkpoint: compared · memory-shape-loadtest: not-compared-no-baseline-frozen

### fraud-loadtest

`baselines/fraud-loadtest.json` → `target/fraud-loadtest.json`

| Metric | Baseline | PR | Δ | Status |
| --- | --- | --- | --- | --- |
| ops/s | 100.00 | 95.00 | -5.00 (-5.0%) | ok |

### memory-shape-loadtest

> **Note (not-compared-no-baseline-frozen):** no baseline at `baselines/memory-shape-loadtest.json` — nothing is frozen to compare against yet.
"""

FAILING_DOC = """# Loadtest baseline comparison

Frozen baselines vs current reports; bands match the `compare-loadtests` gate.

**Status:** warning

**Pairs:** fraud-loadtest: compared (failing rows) · identity-loadtest: compared · fraud-checkpoint: compared · memory-shape-loadtest: not-compared-no-baseline-frozen

### fraud-loadtest

`baselines/fraud-loadtest.json` → `target/fraud-loadtest.json`

| Metric | Baseline | PR | Δ | Status |
| --- | --- | --- | --- | --- |
| errors | 0 | 1 | +1 | fail (1 errors) |
"""

RENDER_ERROR_DOC = """# Loadtest baseline comparison

Frozen baselines vs current reports; bands match the `compare-loadtests` gate.

**Status:** warning

**Pairs:** fraud-loadtest: render-error · identity-loadtest: compared · fraud-checkpoint: report-missing · memory-shape-loadtest: not-compared-no-baseline-frozen

### fraud-loadtest

`baselines/fraud-loadtest.json` → `target/fraud-loadtest.json`

> **Warning (render-error):** comparator exited 1: no diagnostic
"""

# The producer escalates an environment mismatch to `warning`; the old
# `**Pairs:**`-token grep read it as a note (part of the review finding).
MISMATCH_DOC = """# Loadtest baseline comparison

Frozen baselines vs current reports; bands match the `compare-loadtests` gate.

**Status:** warning

**Pairs:** fraud-loadtest: compared (environment mismatch) · identity-loadtest: compared · fraud-checkpoint: compared · memory-shape-loadtest: not-compared-no-baseline-frozen
"""

# A hand-made document with no `**Status:**` line at all (old format).
NO_STATUS_DOC = """# Loadtest baseline comparison

Frozen baselines vs current reports; bands match the `compare-loadtests` gate.

**Pairs:** fraud-loadtest: compared · identity-loadtest: compared · fraud-checkpoint: compared · memory-shape-loadtest: not-compared-no-baseline-frozen
"""

# A status token outside the `ok | warning` vocabulary is producer
# contract drift: fail loud, never silently healthy.
UNKNOWN_STATUS_DOC = """# Loadtest baseline comparison

Frozen baselines vs current reports; bands match the `compare-loadtests` gate.

**Status:** fine-actually

**Pairs:** fraud-loadtest: compared
"""

# The `**Status:**` line is authoritative: this document's `**Pairs:**`
# summary claims failing rows while the producer's status line says ok.
# The updater obeys the status line — the RoundTripTests keep the real
# producer from ever writing the two inconsistently.
STATUS_OK_OVER_FAILING_PAIRS_DOC = """# Loadtest baseline comparison

Frozen baselines vs current reports; bands match the `compare-loadtests` gate.

**Status:** ok

**Pairs:** fraud-loadtest: compared (failing rows) · identity-loadtest: compared
"""


class UpdatePrCommentTestBase(unittest.TestCase):
    """Sandbox: stubbed gh first on PATH, hermetic coreutils, state dir."""

    def setUp(self) -> None:
        if BASH is None:
            self.fail("bash is required to run update-pr-comment.sh")
        self._tmp = tempfile.TemporaryDirectory()
        self.sandbox = Path(self._tmp.name)
        self.state = self.sandbox / "state"
        self.bin = self.sandbox / "bin"
        self.stubbin = self.sandbox / "stubbin"
        for path in (self.state, self.bin, self.stubbin):
            path.mkdir()
        for tool in COREUTILS:
            found = shutil.which(tool)
            if found is None:
                self.fail(f"required tool not found on PATH: {tool}")
            (self.bin / tool).symlink_to(found)
        # The shim's shebang is absolute, so the stub never depends on
        # python3 being on the child's (hermetic) PATH.
        shim = self.stubbin / "gh"
        shim.write_text(
            "#!/bin/sh\n"
            f'exec {shlex.quote(sys.executable)} {shlex.quote(str(GH_STUB))} "$@"\n'
        )
        shim.chmod(0o755)
        self.doc = self.sandbox / "loadtest-comparison.md"
        self.write_comments([])

    def tearDown(self) -> None:
        self._tmp.cleanup()

    # --- fixture and state helpers ------------------------------------

    def write_comments(self, comments: list[dict]) -> None:
        (self.state / "comments.json").write_text(json.dumps(comments))

    def bot_comment(self, comment_id: int, body: str) -> dict:
        """A comment authored by github-actions[bot] (the identity the
        job's GITHUB_TOKEN posts as)."""
        return {"id": comment_id, "body": body, "user": {"login": BOT}}

    def human_comment(self, comment_id: int, login: str, body: str) -> dict:
        return {"id": comment_id, "body": body, "user": {"login": login}}

    def comments_state(self) -> list[dict]:
        return json.loads((self.state / "comments.json").read_text())

    def comment_bodies(self) -> list[str]:
        return [comment["body"] for comment in self.comments_state()]

    def comment_by_id(self, comment_id: int) -> dict:
        for comment in self.comments_state():
            if comment["id"] == comment_id:
                return comment
        self.fail(f"no comment with id {comment_id} in the stub state")

    def calls(self) -> list[dict]:
        log = self.state / "calls.log"
        if not log.exists():
            return []
        return [json.loads(line) for line in log.read_text().splitlines()]

    def writes(self, method: str) -> list[dict]:
        return [call for call in self.calls() if call["method"] == method]

    # --- runners -------------------------------------------------------

    def run_script(
        self,
        *args: str,
        extra_env: dict[str, str] | None = None,
        unset: tuple[str, ...] = (),
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        for key in POPPED:
            env.pop(key, None)
        env.update(BASE_ENV)
        for key in unset:
            env.pop(key, None)
        env["GH_STUB_STATE"] = str(self.state)
        # gh (the stub shim) always sits first on the child PATH; the one
        # test that needs a gh-less PATH overrides it via extra_env, which
        # lands after this line and therefore wins.
        env["PATH"] = f"{self.stubbin}{os.pathsep}{self.bin}"
        if extra_env:
            env.update(extra_env)
        return subprocess.run(
            [BASH, str(SCRIPT), *args],
            text=True,
            capture_output=True,
            check=False,
            env=env,
        )

    def posted_body(
        self,
        *args: str,
        document: str | None = None,
        extra_env: dict[str, str] | None = None,
        unset: tuple[str, ...] = (),
    ) -> str:
        """Run the updater against a marker-comment-less PR (so it POSTs
        rather than PATCHes) and return the one POSTed comment body."""
        if document is not None:
            self.doc.write_text(document)
        result = self.run_script(*args, extra_env=extra_env, unset=unset)
        self.assertEqual(result.returncode, 0, result.stderr)
        posts = self.writes("POST")
        self.assertEqual(len(posts), 1)
        return posts[0]["body"]


class FindAndUpdateTests(UpdatePrCommentTestBase):
    """A bot-authored marker comment already on the PR is PATCHed in place."""

    def test_updates_existing_marker_comment_in_place(self) -> None:
        self.write_comments(
            [
                self.human_comment(11, "alice", "unrelated comment"),
                self.bot_comment(4242, MARKER + "\nprevious run's body"),
                self.human_comment(13, "bob", "another unrelated comment"),
            ]
        )
        self.doc.write_text(CLEAN_DOC)
        result = self.run_script("42", "--doc", str(self.doc))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "update-pr-comment: updated comment 4242 on PR #42", result.stdout
        )
        self.assertEqual(result.stderr, "")
        # one list call, then one PATCH of the bot's marker comment; no create
        calls = self.calls()
        self.assertEqual([call["method"] for call in calls], ["GET", "PATCH"])
        self.assertEqual(
            calls[0]["endpoint"], f"repos/{REPO}/issues/42/comments"
        )
        self.assertTrue(calls[0]["paginate"])
        self.assertEqual(calls[0]["jq"], LIST_FILTER)
        self.assertEqual(
            calls[1]["endpoint"], f"repos/{REPO}/issues/comments/4242"
        )
        body = calls[1]["body"]
        self.assertTrue(body.startswith(MARKER + "\n"), body)
        self.assertIn(f"- **Run:** {RUN_URL}\n", body)
        self.assertIn(f"- **Commit:** {SHA}\n", body)
        self.assertIn(
            "> **Note:** loadtest comparison completed with no failing rows.",
            body,
        )
        self.assertIn("| ops/s | 100.00 | 95.00 | -5.00 (-5.0%) | ok |", body)
        self.assertIn("\n# Loadtest baseline comparison\n", body)
        # the marker comment was replaced in place; the others are intact
        bodies = self.comment_bodies()
        self.assertEqual(bodies[1], body)
        self.assertEqual(bodies[0], "unrelated comment")
        self.assertEqual(bodies[2], "another unrelated comment")
        self.assertEqual(self.writes("POST"), [])

    def test_creates_once_then_updates_in_place_on_rerun(self) -> None:
        self.write_comments([self.human_comment(7, "alice", "looks good")])
        self.doc.write_text(CLEAN_DOC)
        first = self.run_script("42", "--doc", str(self.doc))
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertIn(
            "update-pr-comment: created comparison comment on PR #42",
            first.stdout,
        )
        self.assertEqual(len(self.writes("POST")), 1)
        self.assertEqual(
            len([b for b in self.comment_bodies() if MARKER in b]), 1
        )
        # the stub (like the real API) attributes the POST to
        # github-actions[bot] — the updater's own author filter must
        # therefore find the comment it just created
        self.assertEqual(self.comment_by_id(8)["user"]["login"], BOT)
        # second run: the stub now serves the created comment as
        # bot-authored, so the updater PATCHes instead of POSTing again
        second = self.run_script("42", "--doc", str(self.doc))
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertIn(
            "update-pr-comment: updated comment 8 on PR #42", second.stdout
        )
        self.assertEqual(len(self.writes("POST")), 1)  # never a duplicate
        self.assertEqual(len(self.writes("PATCH")), 1)
        self.assertEqual(len(self.comment_bodies()), 2)


class AuthorFilterTests(UpdatePrCommentTestBase):
    """Only a github-actions[bot] marker comment is ever PATCHed."""

    def test_third_party_marker_comment_is_untouched(self) -> None:
        # a maintainer quoted the marker while debugging; the bot must
        # not edit their comment, and the bot's own comment still
        # updates in place
        squatter = MARKER + "\nwhy does the bot keep doing this? — alice"
        self.write_comments(
            [
                self.human_comment(31, "alice", squatter),
                self.bot_comment(77, MARKER + "\nprevious run's body"),
            ]
        )
        self.doc.write_text(FAILING_DOC)
        result = self.run_script("42", "--doc", str(self.doc))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("update-pr-comment: updated comment 77 on PR #42", result.stdout)
        calls = self.calls()
        self.assertEqual([call["method"] for call in calls], ["GET", "PATCH"])
        # the PATCH targets the bot's comment only
        self.assertEqual(calls[1]["endpoint"], f"repos/{REPO}/issues/comments/77")
        # the third-party marker comment is untouched, author included
        self.assertEqual(self.comment_by_id(31)["body"], squatter)
        self.assertEqual(self.comment_by_id(31)["user"]["login"], "alice")
        # the bot comment carries the new body
        self.assertEqual(self.comment_by_id(77)["body"], calls[1]["body"])
        self.assertEqual(self.writes("POST"), [])

    def test_third_party_marker_comment_alone_gets_a_fresh_post(self) -> None:
        # someone squatted the marker; no bot marker comment exists. The
        # safe behavior: never PATCH theirs — POST a fresh bot comment
        # (and on rerun, that bot comment is the one updated in place).
        squatter = MARKER + "\nofficial comparison, trust me"
        self.write_comments([self.human_comment(31, "eve", squatter)])
        self.doc.write_text(CLEAN_DOC)
        result = self.run_script("42", "--doc", str(self.doc))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "update-pr-comment: created comparison comment on PR #42",
            result.stdout,
        )
        self.assertEqual(self.writes("PATCH"), [])
        # the squatter's comment is untouched
        self.assertEqual(self.comment_by_id(31)["body"], squatter)
        # the fresh comment is the bot's, marker-first
        created = self.comment_by_id(32)
        self.assertEqual(created["user"]["login"], BOT)
        self.assertTrue(created["body"].startswith(MARKER + "\n"), created["body"])
        # exactly one bot-authored marker comment now exists
        bot_markers = [
            c
            for c in self.comments_state()
            if c["user"]["login"] == BOT and MARKER in c["body"]
        ]
        self.assertEqual(len(bot_markers), 1)


class PrNumberTests(UpdatePrCommentTestBase):
    """Where the PR number comes from, and the no-PR no-op."""

    def test_pr_number_from_env(self) -> None:
        self.doc.write_text(CLEAN_DOC)
        result = self.run_script("--doc", str(self.doc), extra_env={"PR_NUMBER": "42"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.calls()[0]["endpoint"], f"repos/{REPO}/issues/42/comments"
        )
        self.assertEqual(len(self.writes("POST")), 1)

    def test_pr_number_argument_overrides_env(self) -> None:
        self.doc.write_text(CLEAN_DOC)
        result = self.run_script(
            "42", "--doc", str(self.doc), extra_env={"PR_NUMBER": "111"}
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.calls()[0]["endpoint"], f"repos/{REPO}/issues/42/comments"
        )

    def test_no_pr_number_no_ops(self) -> None:
        self.doc.write_text(CLEAN_DOC)
        result = self.run_script()  # no positional, PR_NUMBER popped
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no pull request number available", result.stdout)
        self.assertIn("not posting a comment", result.stdout)
        self.assertEqual(result.stderr, "")
        self.assertEqual(self.calls(), [])  # gh was never invoked
        self.assertEqual(self.comment_bodies(), [])


class BestEffortTests(UpdatePrCommentTestBase):
    """A comment that cannot be posted must never fail the CI job."""

    def test_missing_gh_exits_zero_with_stderr(self) -> None:
        # a PATH without the stub dir (extra_env lands after the runner's
        # own PATH composition, so the override wins) means no gh at all
        self.doc.write_text(CLEAN_DOC)
        result = self.run_script(
            "42", "--doc", str(self.doc), extra_env={"PATH": str(self.bin)}
        )
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("gh CLI not found on PATH", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_missing_repository_env_exits_zero(self) -> None:
        self.doc.write_text(CLEAN_DOC)
        result = self.run_script(
            "42", "--doc", str(self.doc), unset=("GITHUB_REPOSITORY",)
        )
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("GITHUB_REPOSITORY is not set", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_gh_api_failure_is_best_effort(self) -> None:
        self.doc.write_text(CLEAN_DOC)
        result = self.run_script(
            "42", "--doc", str(self.doc), extra_env={"GH_STUB_MODE": "fail"}
        )
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("listing comments on PR #42 failed", result.stderr)
        self.assertIn("gh-stub: simulated network failure", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_patch_failure_is_best_effort(self) -> None:
        self.write_comments(
            [self.bot_comment(4242, MARKER + "\nold body")]
        )
        self.doc.write_text(CLEAN_DOC)
        result = self.run_script(
            "42", "--doc", str(self.doc), extra_env={"GH_STUB_MODE": "fail_write"}
        )
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("updating comment 4242 on PR #42 failed", result.stderr)
        self.assertIn("gh-stub: simulated network failure", result.stderr)

    def test_post_failure_is_best_effort(self) -> None:
        self.doc.write_text(CLEAN_DOC)
        result = self.run_script(
            "42", "--doc", str(self.doc), extra_env={"GH_STUB_MODE": "fail_write"}
        )
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("creating a comparison comment on PR #42 failed", result.stderr)
        self.assertIn("gh-stub: simulated network failure", result.stderr)

    def test_missing_document_posts_fallback_body(self) -> None:
        # no document written: the render step produced nothing
        result = self.run_script("42", "--doc", str(self.doc))
        self.assertEqual(result.returncode, 0, result.stderr)
        posts = self.writes("POST")
        self.assertEqual(len(posts), 1)
        body = posts[0]["body"]
        self.assertTrue(body.startswith(MARKER + "\n"), body)
        self.assertIn("is missing", body)
        self.assertIn(str(self.doc), body)
        self.assertIn(f"- **Run:** {RUN_URL}\n", body)
        self.assertEqual(
            len([b for b in self.comment_bodies() if MARKER in b]), 1
        )


class StatusLineTests(UpdatePrCommentTestBase):
    """The overall status line comes from the document's producer-owned
    `**Status:**` line — never from the `**Pairs:**` tokens."""

    def test_warning_status_when_document_warns(self) -> None:
        body = self.posted_body("42", "--doc", str(self.doc), document=FAILING_DOC)
        self.assertIn(
            "> **Warning:** loadtest comparison reported problems",
            body,
        )
        self.assertNotIn("**Note:** loadtest comparison completed", body)

    def test_warning_status_when_render_error(self) -> None:
        body = self.posted_body(
            "42", "--doc", str(self.doc), document=RENDER_ERROR_DOC
        )
        self.assertIn(
            "> **Warning:** loadtest comparison reported problems",
            body,
        )
        self.assertNotIn("**Note:** loadtest comparison completed", body)

    def test_warning_status_on_environment_mismatch(self) -> None:
        # the producer escalates a mismatch to warning; the old
        # `**Pairs:**`-token grep read it as a note
        body = self.posted_body("42", "--doc", str(self.doc), document=MISMATCH_DOC)
        self.assertIn(
            "> **Warning:** loadtest comparison reported problems",
            body,
        )
        self.assertNotIn("**Note:** loadtest comparison completed", body)

    def test_note_status_when_document_is_ok(self) -> None:
        body = self.posted_body("42", "--doc", str(self.doc), document=CLEAN_DOC)
        self.assertIn(
            "> **Note:** loadtest comparison completed with no failing rows.",
            body,
        )
        self.assertNotIn("**Warning:**", body)

    def test_missing_status_line_falls_back_to_warning(self) -> None:
        # recorded fallback choice: a document with no `**Status:**` line
        # (hand-made or old format) is a warning — fail loud in
        # presentation, never silently healthy
        body = self.posted_body(
            "42", "--doc", str(self.doc), document=NO_STATUS_DOC
        )
        self.assertIn(
            "> **Warning:** loadtest comparison reported problems",
            body,
        )
        self.assertNotIn("**Note:** loadtest comparison completed", body)

    def test_unrecognized_status_token_falls_back_to_warning(self) -> None:
        body = self.posted_body(
            "42", "--doc", str(self.doc), document=UNKNOWN_STATUS_DOC
        )
        self.assertIn(
            "> **Warning:** loadtest comparison reported problems",
            body,
        )
        self.assertNotIn("**Note:** loadtest comparison completed", body)

    def test_status_line_is_authoritative_over_pairs_tokens(self) -> None:
        # the producer's `**Status:**` line wins even when the
        # `**Pairs:**` summary disagrees — the updater never greps the
        # pairs tokens (the RoundTripTests keep the real producer honest)
        body = self.posted_body(
            "42", "--doc", str(self.doc), document=STATUS_OK_OVER_FAILING_PAIRS_DOC
        )
        self.assertIn(
            "> **Note:** loadtest comparison completed with no failing rows.",
            body,
        )
        self.assertNotIn("**Warning:**", body)


class MemoryControlsCommentTests(UpdatePrCommentTestBase):
    """--marker and --title keep the loadtest defaults and add the
    memory-controls status tokens."""

    def test_marker_and_title_and_improvement_status(self) -> None:
        marker = "<!-- relate:memory-controls -->"
        document = """# Memory controls and workspace memory calibration

**Status:** improvement

## connection-cap — graph_rss_bytes

**Verdict:** improvement
"""
        body = self.posted_body(
            "42",
            "--doc",
            str(self.doc),
            "--marker",
            marker,
            "--title",
            "Memory controls and workspace memory calibration",
            document=document,
        )
        self.assertTrue(body.startswith(marker + "\n"), body)
        self.assertIn("**Memory controls and workspace memory calibration**", body)
        self.assertIn("> **Warning:** memory controls reported improvement.", body)
        self.assertNotIn(MARKER, body)

    def test_memory_ok_uses_the_held_note(self) -> None:
        document = """# Memory controls and workspace memory calibration

**Status:** ok

All memory controls and workspace memory calibrations held.
"""
        body = self.posted_body(
            "42",
            "--doc",
            str(self.doc),
            "--marker",
            "<!-- relate:memory-controls -->",
            "--title",
            "Memory controls and workspace memory calibration",
            document=document,
        )
        self.assertIn(
            "> **Note:** All memory controls and workspace memory calibrations held.",
            body,
        )


class HeaderTests(UpdatePrCommentTestBase):
    """Run URL and commit SHA lines in the comment header."""

    def test_header_carries_run_url_and_commit(self) -> None:
        body = self.posted_body("42", "--doc", str(self.doc), document=CLEAN_DOC)
        self.assertIn(f"- **Run:** {RUN_URL}\n", body)
        self.assertIn(f"- **Commit:** {SHA}\n", body)

    def test_header_omits_absent_run_and_commit(self) -> None:
        body = self.posted_body(
            "42",
            "--doc",
            str(self.doc),
            document=CLEAN_DOC,
            unset=("GITHUB_SERVER_URL", "GITHUB_RUN_ID", "GITHUB_SHA"),
        )
        self.assertNotIn("**Run:**", body)
        self.assertNotIn("**Commit:**", body)
        self.assertIn(MARKER, body)  # still a complete, postable body


class DocPathTests(UpdatePrCommentTestBase):
    """Document resolution: --doc beats COMPARISON_OUT beats REPORTS_DIR."""

    def test_doc_flag_overrides_comparison_out(self) -> None:
        self.doc.write_text(CLEAN_DOC)
        other = self.sandbox / "other.md"
        other.write_text(FAILING_DOC)
        body = self.posted_body(
            "42",
            f"--doc={self.doc}",  # also covers the --doc=PATH spelling
            extra_env={"COMPARISON_OUT": str(other)},
        )
        self.assertIn("| ops/s | 100.00 | 95.00 |", body)
        self.assertNotIn("| errors | 0 | 1 |", body)

    def test_comparison_out_overrides_reports_dir(self) -> None:
        reports = self.sandbox / "reports"
        reports.mkdir()
        (reports / "loadtest-comparison.md").write_text(FAILING_DOC)
        self.doc.write_text(CLEAN_DOC)
        body = self.posted_body(
            "42",
            extra_env={
                "REPORTS_DIR": str(reports),
                "COMPARISON_OUT": str(self.doc),
            },
        )
        self.assertIn("| ops/s | 100.00 | 95.00 |", body)
        self.assertNotIn("| errors | 0 | 1 |", body)

    def test_reports_dir_locates_default_document(self) -> None:
        reports = self.sandbox / "reports"
        reports.mkdir()
        (reports / "loadtest-comparison.md").write_text(CLEAN_DOC)
        body = self.posted_body("42", extra_env={"REPORTS_DIR": str(reports)})
        self.assertIn("| ops/s | 100.00 | 95.00 |", body)


class RoundTripTests(UpdatePrCommentTestBase):
    """The anchor: the REAL aggregator renders a REAL pair through the
    REAL comparator, and the REAL updater posts the REAL document. If
    any layer of the comparator → aggregator → updater token chain
    renames its vocabulary, this fails instead of drifting silently."""

    def setUp(self) -> None:
        super().setUp()
        self.baselines = self.sandbox / "baselines"
        self.reports = self.sandbox / "reports"
        self.baselines.mkdir()
        self.reports.mkdir()

    def write_baseline(self, name: str, doc: dict) -> None:
        (self.baselines / name).write_text(json.dumps(doc))

    def write_report(self, name: str, doc: dict) -> None:
        (self.reports / name).write_text(json.dumps(doc))

    def render_document(self) -> Path:
        """Run the real `compare-baselines.sh --render-only` over the
        fixture tree; return the document it wrote."""
        env = os.environ.copy()
        env.pop("GITHUB_STEP_SUMMARY", None)
        env.pop("GITHUB_ACTIONS", None)
        env["BASELINES"] = str(self.baselines)
        env["REPORTS_DIR"] = str(self.reports)
        env["PYTHON"] = sys.executable
        result = subprocess.run(
            [BASH, str(AGGREGATOR), "--render-only"],
            text=True,
            capture_output=True,
            check=False,
            env=env,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = self.reports / "loadtest-comparison.md"
        self.assertTrue(doc.exists(), "the aggregator wrote no document")
        return doc

    def test_failing_pair_round_trips_to_a_warning_comment(self) -> None:
        self.write_baseline("fraud-loadtest.json", report(p99=1_000_000))
        self.write_report(
            "fraud-loadtest.json",
            report(
                p99=1_300_000,
                ops_per_sec=50.0,
                logical_used_bytes=2000,
                errors=1,
            ),
        )
        doc = self.render_document()
        document = doc.read_text()
        # the real document escalates the failing pair
        self.assertIn("**Status:** warning\n", document)
        self.assertIn("fraud-loadtest: compared (failing rows)", document)
        self.assertIn("| errors | 0 | 1 | +1 | fail (1 errors) |", document)
        # and the real updater posts it with the warning status and the
        # failing table intact
        body = self.posted_body("42", "--doc", str(doc))
        self.assertIn(
            "> **Warning:** loadtest comparison reported problems", body
        )
        self.assertNotIn("> **Note:**", body)
        self.assertIn("| errors | 0 | 1 | +1 | fail (1 errors) |", body)
        self.assertIn("**Status:** warning\n", body)  # document verbatim

    def test_clean_pair_round_trips_to_a_note_comment(self) -> None:
        self.write_baseline("fraud-loadtest.json", report(p99=1_000_000))
        self.write_report(
            "fraud-loadtest.json",
            report(p99=1_100_000, ops_per_sec=95.0, logical_used_bytes=1050),
        )
        doc = self.render_document()
        document = doc.read_text()
        self.assertIn("**Status:** ok\n", document)
        body = self.posted_body("42", "--doc", str(doc))
        self.assertIn(
            "> **Note:** loadtest comparison completed with no failing rows.",
            body,
        )
        self.assertNotIn("**Warning:**", body)


class UsageTests(UpdatePrCommentTestBase):
    """Only usage errors exit non-zero."""

    def test_usage_errors_exit_two(self) -> None:
        for args in (["--nope"], ["--doc"], ["42", "43"], ["not-a-number"]):
            with self.subTest(args=args):
                result = self.run_script(*args)
                self.assertEqual(result.returncode, 2)
                self.assertIn(
                    "usage: update-pr-comment.sh [<pr-number>] [--doc PATH]",
                    result.stderr,
                )


if __name__ == "__main__":
    unittest.main()
