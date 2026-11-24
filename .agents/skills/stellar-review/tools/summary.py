#!/usr/bin/env python3
"""Stellar-review summary comment: the round summary's renderer
(extracted from post.py, REL8-1769).

`build_summary` renders the round's summary comment, `fmt_summary_line`
renders one finding line, and `verdict_comment` spells the enforce gate's
tier-2 marker. `Outcome` and `Disposition` are the typed round result the
renderer reads. The ledger block embedded in the comment is encoded by
ledger.py's codec. The module also owns the rendering vocabulary the
renderer and the poster share — the severity→GitHub-label map and the
summary marker. post.py (the gh client, the findings policy and the
commands) imports these; the split keeps post.py under the 1k-line
ceiling.
"""

from __future__ import annotations

import enum
import os
import sys
from dataclasses import dataclass

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ledger as ledger_mod  # noqa: E402
from ledger import encode_ledger, end_line, partition  # noqa: E402

SEVERITY_TO_GH = {"blocker": "High", "medium": "Medium", "nit": "Low"}
SUMMARY_MARKER = "**Stellar review:**"


class Outcome(enum.Enum):
    """The round's one outcome, decided once (`round_outcome`) and
    dispatched once by the renderer. PARTIAL is the LABS-635 timeout
    salvage mode: it fails the check whatever the findings say."""
    PASS = "pass"
    PARTIAL = "partial"
    FAIL = "fail"

    @property
    def verdict(self) -> str:
        """The enforce gate's verdict word: only PASS passes."""
        return "pass" if self is Outcome.PASS else "fail"


def round_outcome(failing: bool, timeout_partial: bool) -> Outcome:
    """A partial review can never green the gate (the uncovered passes
    might hold blockers), and its outcome line is its own."""
    if timeout_partial:
        return Outcome.PARTIAL
    return Outcome.FAIL if failing else Outcome.PASS


@dataclass(frozen=True)
class Disposition:
    """The round's finding dispositions as the renderer reads them.
    `new`/`carried`/`resolved`/`waived` come from
    `ledger.round_dispositions`; the rest are cmd_post's splits.
    new/carried/fail_new/low_new/unanchored/anchored_carried are entry
    lists, `resolved` is ids, `waived` entries, `rejected` the ids the
    review API refused inline."""
    new: list[dict]
    carried: list[dict]
    resolved: list[str]
    waived: list[dict]
    fail_new: list[dict]
    low_new: list[dict]
    unanchored: list[dict]
    anchored_carried: list[dict]
    dropped_nits: int
    dropped_scope: int
    rejected: set[str]

    @property
    def open_any(self) -> bool:
        """Whether any finding is open in the ledger, anchored or not:
        every set that can hold one (a degraded round's findings are open
        although the shown bucket is empty; context and carried findings
        are open in the ledger)."""
        return bool(self.fail_new or self.low_new or self.carried or self.unanchored)

    @property
    def open_gating(self) -> int:
        """The count of open findings on this PR's own diff — anchored
        new plus anchored carried (context findings never gate)."""
        return len(self.fail_new) + len(self.low_new) + len(self.anchored_carried)


def fmt_summary_line(f: dict, with_remedy: bool = True) -> str:
    sev = SEVERITY_TO_GH[f["severity"]]
    end = end_line(f["lines"])
    fix = f" _Fix:_ {f['remedy']}" if with_remedy and f.get("remedy") else ""
    return (f"- **[{sev}] {f['area']} ({f['checklist_id']}) {f['id']}:** "
            f"`{f['path']}:{f['lines'][0]}-{end}` — {f['summary']}{fix}")


def verdict_comment(verdict: str) -> str:
    """The enforce gate's tier-2 wire format (enforce-opencode-verdict.sh
    matches exactly this shape), the one derivation: every summary
    assembler — stellar's and the per-kind lanes' — appends its output
    instead of re-spelling the HTML comment, so a format change lands
    once and every lane's tier-2 signal moves with it."""
    return f"<!-- opencode-verdict: {verdict} -->"


def build_summary(facts: dict, scope_stats: dict | None, round_no: int, head: str,
                  outcome: Outcome, led: dict, disposition: Disposition) -> str:
    """Render the summary comment from the round's typed `outcome` and
    `disposition`. New summary elements grow `Disposition`, not the
    parameter list; the inline re-bucketing and its heading are decided
    here, in one place."""
    # ADR-0346: "New findings" shows the diff-anchored round findings only;
    # context-unit findings appear exactly once, in their own section below,
    # never merged into the rejected bucket — "context" (by design, never
    # posted inline) and "the review API rejected inline for" (counts in
    # the verdict) are different facts with different headings.
    # ADR-0354: the severity halves shipped in the disposition — computed
    # once in cmd_post. Fail-severity (blocker/medium) findings render in
    # the New-findings / rejected sections and thread inline; nits render
    # once, in the collapsed appendix, and never thread.
    d = disposition
    # A finding lands in "rejected inline" only when the API refused the
    # batch it rode in — the rejected-id set names exactly those, and a
    # finding with no diff anchor to attempt was never in it, so it can
    # never be misnamed as rejected.
    degraded_new, shown_new = partition(d.fail_new, lambda f: f["id"] in d.rejected)
    total = scope_stats["units_total"] if scope_stats else len(facts["units"])
    in_scope = scope_stats["units_in_scope"] if scope_stats else total
    lines = [
        "**Stellar review** (stellar-review skill)",
        "",
        f"Round {round_no} · head `{head[:8]}` · scope: {in_scope}/{total} units "
        f"({total - in_scope} sealed from earlier rounds).",
        "",
    ]

    def section(title: str, note: str, items: list[dict],
                with_remedy: bool = True) -> None:
        """One titled finding list under a bold heading."""
        if not items:
            return
        lines.append("")
        lines.append(f"**{title}**" + (f" ({note})" if note else ""))
        lines.extend(fmt_summary_line(f, with_remedy) for f in items)

    def collapsed_section(title: str, summary_note: str, items: list[dict]) -> None:
        """One finding list as an HTML <details> block (ADR-0354's Low
        appendix shape)."""
        if not items:
            return
        lines.extend(["", "<details>", f"<summary>{title} — {summary_note}</summary>", ""])
        lines.extend(fmt_summary_line(f) for f in items)
        lines.extend(["", "</details>"])

    section("New findings", "", shown_new)
    section("Findings the review API rejected inline for",
            "summary only — they still count in the verdict; see run log",
            degraded_new)
    section("Findings on context units",
            "not on the current diff — summary only; night-shift backlog, "
            "never inline, never verdict blockers",
            d.unanchored)
    # carried entries keep their one rendering difference: no remedy line
    section("Still open from earlier rounds", "", d.carried, with_remedy=False)
    # ADR-0354: anchored nits never open inline threads — they print once,
    # collapsed; the next round's list supersedes them automatically
    collapsed_section("Low findings",
                      "collapsed, no inline threads (ADR-0354); "
                      "superseded by the next round's list", d.low_new)
    trail = []
    if d.resolved:
        trail.append("**Resolved:** " + ", ".join(d.resolved))
    if d.waived:
        trail.append("**Waived:** " + ", ".join(
            f"{w['id']} ({w.get('waive_reason', '')})" for w in d.waived))
    if d.dropped_nits:
        trail.append(f"**Nits dropped by the cap:** {d.dropped_nits}")
    if d.dropped_scope:
        trail.append(f"**Findings dropped by the scope/incremental filter:** "
                     f"{d.dropped_scope}"
                     " (out-of-scope unit, or unchanged lines in an incremental round — "
                     "see run log)")
    if trail:
        lines += ["", " · ".join(trail)]
    # a clean salvage round is not "No open issues." — the partial
    # outcome line below is that round's outcome, unqualified
    if not d.open_any and outcome is not Outcome.PARTIAL:
        lines.append("No open issues.")
    lines.append("")
    if outcome is Outcome.PASS:
        lines.append(f"{SUMMARY_MARKER} ✅ no issues found")
    elif outcome is Outcome.PARTIAL:
        # LABS-635 salvage mode, the fail arm's own outcome: the
        # findings above are the passes a timed-out round completed; the
        # uncovered passes could still hold blockers, so the check fails
        # regardless of the count above.
        lines.append(f"{SUMMARY_MARKER} ⚠️ partial round — the model run hit "
                     f"the review timeout; the passes not covered could "
                     f"still hold blockers, so this check fails and the "
                     f"next push or re-run reviews fresh")
    else:
        lines.append(f"{SUMMARY_MARKER} ⚠️ {d.open_gating} issue(s) open "
                     f"({len(d.fail_new)} + {len(d.low_new)} anchored new (fail/low) + "
                     f"{len(d.anchored_carried)} carried)")
    lines.append(verdict_comment(outcome.verdict))
    trimmed = ledger_mod.trim_for_comment(led)
    lines.append(f"<!-- stellar-ledger: {encode_ledger(trimmed)} -->")
    return "\n".join(lines) + "\n"
