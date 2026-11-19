"""Drift guard for the stellar-review plugin consumption (ADR-0623): the
plugin (npm opencode-stellar-review) owns the review pipeline, and this
repository's wiring must stay coherent — no stale references to the
deleted vendored copy, every lane resolving the plugin through the one
pinned install, the model-tier overrides flowing as STELLAR_MODEL_*
environment into the run step (the pass_models.py config writer is gone),
the post steps pinning STELLAR_ROOT from runner-provided values instead
of the GITHUB_ENV-published copy, and the version pin spelled once per
workflow that installs it, all in agreement. Run via make test-scripts
and the review-scripts-test workflow."""

import os
import re
import unittest

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))

REUSABLE = os.path.join(ROOT, ".github", "workflows", "opencode-review-reusable.yml")
STELLAR_LANE = os.path.join(ROOT, ".github", "workflows", "opencode-review-stellar.yml")
NIGHTSHIFT = os.path.join(ROOT, ".github", "workflows", "stellar-nightshift.yml")
SCRIPTS_TEST = os.path.join(ROOT, ".github", "workflows", "review-scripts-test.yml")
PREP = os.path.join(ROOT, ".github", "scripts", "opencode-review-prep.sh")
POSTER = os.path.join(ROOT, ".github", "scripts", "opencode-review-post.py")
AGENTS = os.path.join(ROOT, "AGENTS.md")
AUTOPILOT = os.path.join(
    ROOT, ".agents", "skills", "agent-backlog-autopilot", "SKILL.md")

# Wiring surfaces that must never name the deleted vendored tree again.
WIRING_SURFACES = [
    REUSABLE, STELLAR_LANE, NIGHTSHIFT, SCRIPTS_TEST, PREP, POSTER,
    AGENTS, AUTOPILOT,
]

VENDORED = re.compile(r"\.agents/skills/stellar-review")
PIN = re.compile(r"opencode-stellar-review@(\d+\.\d+\.\d+)")


def read(path):
    with open(path) as fh:
        return fh.read()


class TestNoVendoredReferences(unittest.TestCase):
    def test_wiring_surfaces_name_the_plugin_not_the_vendored_tree(self):
        offenders = []
        for path in WIRING_SURFACES:
            rel = os.path.relpath(path, ROOT)
            for n, line in enumerate(read(path).splitlines(), 1):
                if VENDORED.search(line):
                    offenders.append(f"{rel}:{n}")
        self.assertEqual(
            offenders, [],
            "stale references to the deleted vendored stellar-review tree "
            "(ADR-0623): " + ", ".join(offenders))

    def test_the_vendored_tree_is_gone(self):
        for gone in (
            os.path.join(ROOT, ".agents", "skills", "stellar-review"),
            os.path.join(ROOT, ".claude", "agents", "stellar-structure.md"),
            os.path.join(ROOT, ".opencode", "agents", "stellar-structure.md"),
            os.path.join(ROOT, ".github", "scripts", "stellar-prep.sh"),
            os.path.join(ROOT, ".github", "prompts",
                         "opencode-review-stellar.md"),
        ):
            self.assertFalse(os.path.exists(gone), gone)

    def test_the_nightshift_family_moved_with_it(self):
        for moved in ("nightshift.py", "linear_queue.py", "passstats.py",
                      "recurrence.py"):
            path = os.path.join(ROOT, ".github", "scripts",
                                "stellar-nightshift", moved)
            self.assertTrue(os.path.exists(path), path)


class TestReusableWorkflowWiring(unittest.TestCase):
    def setUp(self):
        self.text = read(REUSABLE)

    def test_installs_the_pinned_plugin_two_ways(self):
        # The npm prefix serves the shell steps; opencode plugin add
        # pre-seeds the CLI cache so the model step loads the skill
        # immediately.
        self.assertIn('npm install --prefix "$RUNNER_TEMP/stellar-review"', self.text)
        self.assertIn('opencode plugin add "opencode-stellar-review@${STELLAR_REVIEW_VERSION}"', self.text)

    def test_publishes_stellar_root_before_the_model_and_pins_it_after(self):
        # Pre-model: a fixed runner-authored GITHUB_ENV write. Post-model
        # (the round-22 finding class): the post steps never trust that
        # copy — they pin the same fixed path from runner.temp in step
        # env, twice (scripted + per-kind), and pass it through both
        # env -i re-exec lists.
        self.assertIn('echo "STELLAR_ROOT=$STELLAR_ROOT" >> "$GITHUB_ENV"', self.text)
        pinned = ('STELLAR_ROOT: ${{ runner.temp }}'
                  "/stellar-review/node_modules/opencode-stellar-review")
        self.assertEqual(self.text.count(pinned), 2,
                         "both post steps must pin STELLAR_ROOT from runner.temp")
        reexec = 'STELLAR_ROOT="$STELLAR_ROOT"'
        self.assertEqual(self.text.count(reexec), 2,
                         "both env -i re-exec lists must pass STELLAR_ROOT through")

    def test_scripted_prep_and_post_run_the_plugin_entry_point(self):
        self.assertIn('"$STELLAR_ROOT/scripts/stellar-prep.sh" model', self.text)
        self.assertIn('"$STELLAR_ROOT/scripts/stellar-prep.sh" post', self.text)

    def test_the_scripted_prompt_header_ships_with_the_plugin(self):
        self.assertIn(
            'cat "$STELLAR_ROOT/templates/ci/opencode-review-stellar.md"',
            self.text)

    def test_pass_model_overrides_flow_as_environment(self):
        # ADR-0623 (the ADR-0302 arm): the run step exports the four
        # STELLAR_MODEL_* vars; the plugin's skill applies them per
        # dispatch. pass_models.py is gone with the vendored copy.
        for var in ("STELLAR_MODEL_STRUCTURE", "STELLAR_MODEL_BRANCHING",
                    "STELLAR_MODEL_BOUNDARIES", "STELLAR_MODEL_COMPARISON"):
            self.assertIn(f"{var}: ${{{{ secrets.{var} || vars.{var} }}}}", self.text)
        self.assertNotIn("pass_models", self.text)

    def test_the_stellar_lane_passes_no_prompt_header(self):
        self.assertNotIn("prompt_header:", read(STELLAR_LANE))


class TestPerKindScriptsResolveThePlugin(unittest.TestCase):
    def test_prep_requires_and_uses_stellar_root(self):
        text = read(PREP)
        self.assertIn(': "${STELLAR_ROOT:?opencode-review-prep.sh: STELLAR_ROOT required}"', text)
        self.assertIn('T="$STELLAR_ROOT/skills/stellar-review/tools"', text)

    def test_poster_resolves_the_core_through_stellar_root(self):
        text = read(POSTER)
        self.assertIn('os.environ.get("STELLAR_ROOT")', text)
        self.assertNotIn('".agents", "skills", "stellar-review"', text)


class TestNightshiftWiring(unittest.TestCase):
    def setUp(self):
        self.text = read(NIGHTSHIFT)

    def test_installs_the_pinned_plugin_in_the_ledger_jobs(self):
        # plan and recurrence call post.py fetch-ledger; each job is its
        # own runner, so each installs.
        self.assertEqual(
            self.text.count("opencode-stellar-review@${STELLAR_REVIEW_VERSION}"),
            2, "both ledger-fetching jobs must install the pinned plugin")

    def test_local_tools_come_from_the_repository_directory(self):
        self.assertIn("T=.github/scripts/stellar-nightshift", self.text)
        self.assertIn("T=/tmp/pristine/.github/scripts/stellar-nightshift", self.text)
        self.assertIn('$STELLAR_ROOT/skills/stellar-review/tools/post.py" fetch-ledger', self.text)


class TestVersionPinsAgree(unittest.TestCase):
    """The pin is spelled once per workflow that installs the plugin
    (reusable, nightshift, review-scripts-test — the last as literals in
    its two install steps); all must name the same version, so a bump
    that forgets one fails here instead of silently splitting lanes
    across plugin versions."""

    def test_all_pins_name_one_version(self):
        versions = set()
        for path in (REUSABLE, NIGHTSHIFT):
            m = re.search(r'STELLAR_REVIEW_VERSION: "(\d+\.\d+\.\d+)"', read(path))
            self.assertIsNotNone(m, f"{os.path.relpath(path, ROOT)}: no STELLAR_REVIEW_VERSION pin")
            versions.add(m.group(1))
        literal = set(PIN.findall(read(SCRIPTS_TEST)))
        self.assertTrue(literal, "review-scripts-test.yml must pin the plugin")
        versions |= literal
        self.assertEqual(
            len(versions), 1,
            f"plugin version pins disagree across workflows: {sorted(versions)}")


if __name__ == "__main__":
    unittest.main()
