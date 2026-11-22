#!/usr/bin/env python3
"""Stellar-review summary comment: the round summary's renderer and the
ledger blob it carries (extracted from post.py, REL8-1769).

`build_summary` renders the round's summary comment, `fmt_summary_line`
renders one finding line, and `verdict_comment` spells the enforce gate's
tier-2 marker. The ledger block embedded in that comment is a
zlib+base64 blob whose one codec is `encode_ledger`/`decode_ledger`. The
module also owns the rendering vocabulary the renderer and the poster
share — the severity→GitHub-label map, the summary marker — plus the
complementary-split helper both read. post.py (the gh client, the
findings policy and the commands) imports these; the split keeps post.py
under the 1k-line ceiling.
"""

from __future__ import annotations

import base64
import json
import os
import re
import sys
import zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ledger as ledger_mod  # noqa: E402

SEVERITY_TO_GH = {"blocker": "High", "medium": "Medium", "nit": "Low"}
SUMMARY_MARKER = "**Stellar review:**"
LEDGER_RE = re.compile(r"<!--\s*stellar-ledger:\s*(\{.*?\}|z:[A-Za-z0-9+/=]+)\s*-->", re.DOTALL)
# The comment-embedded ledger is ALWAYS zlib+base64 ("z:" arm): the
# plaintext arm would embed unit ids (attacker-controlled filenames)
# verbatim, and a crafted filename could smuggle a forged
# `<!-- stellar-ledger: z:... -->` block into the comment. Base64 never
# contains the HTML comment markers.
LEDGER_COMPRESS_OVER = 0


LEDGER_BUDGET = 48000  # leave room for the summary text under 65536


def partition(items: list[dict], pred) -> tuple[list[dict], list[dict]]:
    """The poster's canonical split shape — every two-bucket site (the
    scope filters, the nit cap, the summary's render buckets) routes
    through it: both halves of the partition in a single pass, so the
    halves cannot drift apart."""
    yes, no = [], []
    for item in items:
        (yes if pred(item) else no).append(item)
    return yes, no


def encode_ledger(led: dict) -> str:
    def enc(d: dict) -> str:
        raw = json.dumps(d, sort_keys=True, separators=(",", ":"))
        return raw if len(raw) <= LEDGER_COMPRESS_OVER else _compressed(raw)

    out = enc(led)
    if len(out) > LEDGER_BUDGET and led.get("units"):
        # extreme PR: drop the unit map (see trim_for_comment docstring)
        led = dict(led, units={}, dropped_units=len(led["units"]))
        out = enc(led)
    return out


def _compressed(raw: str) -> str:
    return "z:" + base64.b64encode(zlib.compress(raw.encode(), 9)).decode()


def decode_ledger(blob: str) -> dict:
    if blob.startswith("z:"):
        blob = zlib.decompress(base64.b64decode(blob[2:])).decode()
    return json.loads(blob)


def fmt_summary_line(f: dict) -> str:
    sev = SEVERITY_TO_GH[f["severity"]]
    end = f["lines"][1] or f["lines"][0]
    fix = f" _Fix:_ {f['remedy']}" if f.get("remedy") else ""
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
                  verdict: str, led: dict, disposition: dict) -> str:
    """Render the summary comment. `disposition` bundles the round's
    outcome — every key the body reads is declared here exactly once:
    fail_new, low_new (the severity halves of the round's new findings),
    carried, anchored_carried, unanchored, resolved, waived,
    dropped_nits, dropped_scope, rejected, timeout_partial. New
    summary elements grow the bundle, not the parameter list; the inline
    re-bucketing and its heading are decided here, in one place."""
    # ADR-0346: "New findings" shows the diff-anchored round findings only;
    # context-unit findings appear exactly once, in their own section below,
    # never merged into the rejected bucket — "context" (by design, never
    # posted inline) and "the review API rejected inline for" (counts in
    # the verdict) are different facts with different headings.
    # ADR-0354: the severity halves shipped in the disposition — computed
    # once in cmd_post. Fail-severity (blocker/medium) findings render in
    # the New-findings / rejected sections and thread inline; nits render
    # once, in the collapsed appendix, and never thread.
    fail_new = disposition["fail_new"]
    low_new = disposition["low_new"]
    carried = disposition["carried"]
    # A finding lands in "rejected inline" only when the API refused the
    # batch it rode in — the rejected-id set names exactly those, and a
    # finding with no diff anchor to attempt was never in it, so it can
    # never be misnamed as rejected.
    rejected = disposition["rejected"]
    degraded_new, shown_new = partition(fail_new, lambda f: f["id"] in rejected)
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
                collapsed: str = "") -> None:
        """One titled finding list. `collapsed` renders the list as an HTML
        <details> block with that summary note instead of a bold heading
        (ADR-0354's Low appendix shape)."""
        if not items:
            return
        lines.append("")
        if collapsed:
            lines.append("<details>")
            lines.append(f"<summary>{title} — {collapsed}</summary>")
            lines.append("")
            lines.extend(fmt_summary_line(f) for f in items)
            lines.append("")
            lines.append("</details>")
            return
        lines.append(f"**{title}**" + (f" ({note})" if note else ""))
        lines.extend(fmt_summary_line(f) for f in items)

    section("New findings", "", shown_new)
    section("Findings the review API rejected inline for",
            "summary only — they still count in the verdict; see run log",
            degraded_new)
    section("Findings on context units",
            "not on the current diff — summary only; night-shift backlog, "
            "never inline, never verdict blockers",
            disposition["unanchored"])
    # carried entries keep their one rendering difference: no remedy line
    section("Still open from earlier rounds", "",
            [{**f, "remedy": None} for f in carried])
    # ADR-0354: anchored nits never open inline threads — they print once,
    # collapsed; the next round's list supersedes them automatically
    section("Low findings", "", low_new,
            collapsed="collapsed, no inline threads (ADR-0354); "
                      "superseded by the next round's list")
    trail = []
    if disposition["resolved"]:
        trail.append("**Resolved:** " + ", ".join(disposition["resolved"]))
    if disposition["waived"]:
        trail.append("**Waived:** " + ", ".join(
            f"{w['id']} ({w.get('waive_reason', '')})" for w in disposition["waived"]))
    if disposition["dropped_nits"]:
        trail.append(f"**Nits dropped by the cap:** {disposition['dropped_nits']}")
    if disposition["dropped_scope"]:
        trail.append(f"**Findings dropped by the scope/incremental filter:** "
                     f"{disposition['dropped_scope']}"
                     " (out-of-scope unit, or unchanged lines in an incremental round — "
                     "see run log)")
    if trail:
        lines += ["", " · ".join(trail)]
    # 'Open' is decided once, from every set that can hold an open
    # finding, spelled from the authoritative lists the outcome logic
    # reads: shown_new/degraded_new are complementary render buckets of
    # fail_new (a degraded round's findings are open even though
    # shown_new is empty), and unanchored context findings are open in
    # the ledger.
    has_open = bool(fail_new or low_new or carried
                    or disposition["anchored_carried"]
                    or disposition["unanchored"])
    # a clean salvage round is not "No open issues." — the partial
    # outcome line below is that round's outcome, unqualified
    if not has_open and not disposition["timeout_partial"]:
        lines.append("No open issues.")
    lines.append("")
    n_open = len(fail_new) + len(low_new) + len(disposition["anchored_carried"])
    if verdict == "pass":
        lines.append(f"{SUMMARY_MARKER} ✅ no issues found")
    elif disposition["timeout_partial"]:
        # LABS-635 salvage mode, the fail arm's own outcome: the
        # findings above are the passes a timed-out round completed; the
        # uncovered passes could still hold blockers, so the check fails
        # regardless of the count above.
        lines.append(f"{SUMMARY_MARKER} ⚠️ partial round — the model run hit "
                     f"the review timeout; the passes not covered could "
                     f"still hold blockers, so this check fails and the "
                     f"next push or re-run reviews fresh")
    else:
        lines.append(f"{SUMMARY_MARKER} ⚠️ {n_open} issue(s) open "
                     f"({len(fail_new)} + {len(low_new)} anchored new (fail/low) + "
                     f"{len(disposition['anchored_carried'])} carried)")
    lines.append(verdict_comment(verdict))
    trimmed = ledger_mod.trim_for_comment(led)
    lines.append(f"<!-- stellar-ledger: {encode_ledger(trimmed)} -->")
    return "\n".join(lines) + "\n"
