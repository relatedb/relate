"""Drift guard for the one local-round entry point: local pre-push
stellar rounds run `.github/scripts/stellar-prep.sh local`, the same
pipeline body CI's `model` mode runs — never a hand-rolled subset of
Step 0 (the abbreviated hand-copy let CI re-find findings the local
round never saw). SKILL.md's Step 0 must lead with the script invocation
and forbid the hand-rolled subset; the script must offer the `local`
mode that invocation depends on and cite Step 0's scope rule instead of
restating it; the quick-mode contract (pre-filter, never a gate; the
full round stays mandatory before every push) must appear in BOTH the
script's usage and SKILL.md in the same words; and the reusable workflow
must keep calling the CI modes — so the two pipelines cannot silently
diverge again."""

import os
import re
import unittest

from helpers import ROOT, read, region_between  # also puts tools/ on sys.path

SKILL = os.path.join(ROOT, ".agents", "skills", "stellar-review", "SKILL.md")
PREP = os.path.join(ROOT, ".github", "scripts", "stellar-prep.sh")
REUSABLE = os.path.join(ROOT, ".github", "workflows", "opencode-review-reusable.yml")
AGENTS = os.path.join(ROOT, "AGENTS.md")

# The one-command local round, as SKILL.md Step 0 must spell it.
LOCAL_INVOCATION = r"\.github/scripts/stellar-prep\.sh local"
# The manual-reference marker: the old hand-copy path, which must stay
# below the one-command lead, never above it.
MANUAL_MARKER = re.compile(r"^TOOLS=", re.M)
# The quick-mode contract, in the same words on every surface that
# states it (stellar-prep.sh's usage comment and SKILL.md Step 0).
QUICK_PREFILTER = re.compile(r"Quick is a PRE-FILTER, never a gate")
QUICK_FULL_ROUND_MANDATORY = re.compile(
    r"the full round \(carried ledger \+ expansion \+ shards\) "
    r"is still mandatory before every push"
)
# The mode-choice comparison, same words on both docs surfaces (SKILL.md
# full version, AGENTS.md one line) — an agent chooses modes by it. The
# sentence-initial "A" is capitalized wherever the sentence opens a
# paragraph; matching runs on whitespace-collapsed prose because AGENTS.md
# hard-wraps lines mid-sentence.
# every `git merge-base ...` call outside comment lines, any number per line
COMMENT_LINE = re.compile(r"(?m)^[ \t]*#.*$")
MERGE_BASE_CALL = re.compile(r"git (merge-base [^;)\n]*)")

COMPARISON = re.compile(
    r"[Aa] clean quick round does NOT predict a clean CI round; "
    r"a clean full round is the best local predictor of CI's verdict"
)


def step0():
    """SKILL.md's Step 0 section, through the next `## ` heading."""
    return region_between(read(SKILL), r"^## Step 0 ", r"^## ", SKILL)


def local_branch():
    """stellar-prep.sh's local-mode input derivation branch."""
    return region_between(
        read(PREP), r'^if \[ "\$MODE" = "local" \]; then', r"^else$", PREP
    )


def unwrapped(text):
    """Whitespace-collapsed text, so a sentence matches across the
    surface's own line wrapping (AGENTS.md wraps mid-sentence)."""
    return re.sub(r"\s+", " ", text)


def comment_prose(path):
    """The leading `#` comment block of a shell script as one prose line
    (shebang, comment padding, and line wraps removed), so a contract
    sentence matches however the comment is wrapped. One normalizer for
    the result: the same whitespace collapse `unwrapped` applies."""
    lines = []
    for line in read(path).splitlines():
        if line.startswith("#!"):
            continue
        if not line.startswith("#"):
            break
        lines.append(line[1:].strip())
    return unwrapped(" ".join(part for part in lines if part))


class TestLocalRoundEntry(unittest.TestCase):
    def test_step0_leads_with_the_script_invocation(self):
        region = step0()
        lead = re.search(LOCAL_INVOCATION, region)
        self.assertIsNotNone(
            lead,
            "SKILL.md Step 0: the local round must be spelled as the "
            "stellar-prep.sh local invocation, not as hand-rolled steps",
        )
        manual = MANUAL_MARKER.search(region)
        self.assertIsNotNone(
            manual, "SKILL.md Step 0: the manual reference block is missing"
        )
        self.assertLess(
            lead.start(),
            manual.start(),
            "SKILL.md Step 0: the one-command local round must lead; the "
            "manual steps are the reference below it",
        )

    def test_step0_forbids_the_hand_rolled_subset(self):
        self.assertRegex(
            step0(),
            r"ALWAYS run the script.*never a hand-rolled subset",
            "SKILL.md Step 0: must state that local rounds always use the "
            "script, never a hand-rolled subset of the steps",
        )

    def test_prep_offers_the_local_mode(self):
        self.assertRegex(
            read(PREP),
            re.escape("usage: stellar-prep.sh model|post|local [--quick] [PR_NUMBER]"),
            "stellar-prep.sh: SKILL.md's local invocation requires a local "
            "mode (with the --quick flag) on the script",
        )

    def test_quick_contract_stated_identically_on_both_surfaces(self):
        # quick is a pre-filter, never a gate: the contract must appear,
        # in the same words, in the script's usage comment AND SKILL.md's
        # local-round section — one surface weakening it fails here
        for label, text in ((PREP, comment_prose(PREP)), (SKILL, unwrapped(step0()))):
            with self.subTest(surface=os.path.basename(label)):
                self.assertRegex(
                    text,
                    QUICK_PREFILTER,
                    f"{label}: quick mode must be stated as a PRE-FILTER, "
                    "never a gate",
                )
                self.assertRegex(
                    text,
                    QUICK_FULL_ROUND_MANDATORY,
                    f"{label}: the full round must be stated as still "
                    "mandatory before every push",
                )

    def test_prep_local_cites_the_step0_scope_rule(self):
        # the scope rule's one spelling lives in SKILL.md Step 0; the
        # script's local branch must cite it, never restate it
        branch = local_branch()
        self.assertRegex(
            branch,
            r"SKILL\.md Step 0",
            "stellar-prep.sh: the local branch must cite SKILL.md Step 0's "
            "scope rule instead of restating it",
        )

    def test_diverged_last_head_falls_back_to_the_main_merge_base(self):
        # LABS-955: a rebased branch's stale last_head must not widen the
        # scope via merge-base(LAST, HEAD)
        branch = local_branch()
        calls = [
            c.strip() for c in MERGE_BASE_CALL.findall(COMMENT_LINE.sub('', branch))
        ]
        self.assertEqual(
            calls,
            ['merge-base --is-ancestor "$LAST" "$HEAD_SHA" 2>/dev/null',
             "merge-base HEAD origin/main"],
        )

    def test_merge_base_scan_catches_a_call_after_a_semicolon(self):
        # a forbidden call appended after `;` on an allowed line must
        # surface as its own extracted call
        line = (
            'git merge-base --is-ancestor "$LAST" "$HEAD_SHA" 2>/dev/null'
            '; git merge-base "$LAST" "$HEAD_SHA"\n'
        )
        self.assertEqual(
            [c.strip() for c in MERGE_BASE_CALL.findall(COMMENT_LINE.sub('', line))],
            ['merge-base --is-ancestor "$LAST" "$HEAD_SHA" 2>/dev/null',
             'merge-base "$LAST" "$HEAD_SHA"'],
        )
        self.assertEqual(MERGE_BASE_CALL.findall(COMMENT_LINE.sub("", "# git merge-base x\n")), [])

    def test_agents_md_leads_with_the_entry_point(self):
        text = read(AGENTS)
        self.assertRegex(
            text,
            r"stellar-prep\.sh local",
            "AGENTS.md: the catch-findings-locally bullet must run the "
            "local round through stellar-prep.sh, not hand-copied steps",
        )
        self.assertRegex(
            text,
            r"the mandatory pre-push gate, not optional",
            "AGENTS.md: the full local round must stay the mandatory "
            "pre-push gate",
        )

    def test_comparison_sentence_on_both_docs_surfaces(self):
        for label, text in ((SKILL, step0()), (AGENTS, read(AGENTS))):
            with self.subTest(surface=os.path.basename(label)):
                self.assertRegex(
                    unwrapped(text),
                    COMPARISON,
                    f"{label}: the quick-vs-full comparison sentence must "
                    "stay, so an agent can choose a mode informed",
                )

    def test_ci_still_calls_the_model_and_post_modes(self):
        text = read(REUSABLE)
        for mode in ("model", "post"):
            with self.subTest(mode=mode):
                self.assertRegex(
                    text,
                    r"(?m)^ *\.github/scripts/stellar-prep\.sh " + mode + r" *$",
                    f"opencode-review-reusable.yml: CI must keep calling "
                    f"stellar-prep.sh {mode}",
                )


if __name__ == "__main__":
    unittest.main()
