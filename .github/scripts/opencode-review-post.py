#!/usr/bin/env python3
"""Per-kind review poster (ADR-0603): the scripted posting path for the
rust and security review lanes.

The model step runs with no GitHub token at all; its only output is
.stellar/findings.json. This script owns everything the model used to do
through gh: it validates the findings document, posts every inline
comment in ONE POST /repos/{repo}/pulls/{pr}/reviews call, upserts the
single summary comment (discover-then-edit), derives the verdict itself
— ADR-0258's severity gate: any Critical/High/Medium finding fails — and
writes .opencode-verdict.txt. The fail-closed chain through
.github/scripts/enforce-opencode-verdict.sh is unchanged.

The GitHub mechanics — the gh wrapper, payload files, the review POST
with its degrade policy, the summary upsert, the verdict write, the
sanitize boundary for model text, and the shared command layer (the
findings-file read-and-diagnose gate, the dry-run dump, the fixed-round
landing, the verdict comment) — are stellar-review's post.py, the one
canonical implementation, shared by import.

Commands:
    gate   Print true/false — whether the kind's no-op gate holds (the
           diff holds no kind-relevant change), from the scripted facts.
    check  Validate a findings document: 'valid' + exit 0, or a distinct
           diagnosis + exit 1 (missing / empty / unparseable / wrong
           shape / bad finding field).
    noop   Post the no-op summary and a pass verdict.
    post   Apply findings: inline comments, summary, verdict.
    fail   Fail-closed summary and a fail verdict.

Usage:
    opencode-review-post.py gate --kind 'Rust review' --facts .stellar/facts.json
    opencode-review-post.py post --kind 'Rust review' --repo owner/name \
        --pr N --head-sha SHA --findings .stellar/findings.json
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import os
import sys
from collections.abc import Callable

# The stellar poster owns the one gh/posting core (the same dependency
# the review lanes' prep already spells); it is imported, not duplicated.
# The core lives in the stellar-review plugin (ADR-0623) — npm
# opencode-stellar-review — and STELLAR_ROOT names the installed package
# root. post.py puts its own directory on sys.path at import, so its
# sibling modules resolve from any import site.
_STELLAR_ROOT = os.environ.get("STELLAR_ROOT")
if not _STELLAR_ROOT:
    raise SystemExit(
        "opencode-review-post.py: STELLAR_ROOT not set — point it at the "
        "installed opencode-stellar-review package root (ADR-0623)"
    )
_STELLAR_TOOLS = os.path.join(_STELLAR_ROOT, "skills", "stellar-review", "tools")
sys.path.insert(0, _STELLAR_TOOLS)
import post as stellar_post  # noqa: E402
import facts as stellar_facts  # noqa: E402

# ADR-0258's severity vocabulary and gate: Critical/High/Medium fail, Low
# passes. The one spelling for these lanes; the marker line, the verdict
# file, and the inline bodies all read these tables.
SEVERITIES = ("Critical", "High", "Medium", "Low")
FAIL_SEVERITIES = frozenset({"Critical", "High", "Medium"})
SEVERITY_RANK = {name: rank for rank, name in enumerate(SEVERITIES)}

REQUIRED_FIELDS = ("path", "line", "severity", "category", "title", "why", "fix")

# The security lane's always-relevant paths: these control what code and
# instructions run in CI with secrets — the workflows and the actions
# they compose, the scripts and prompts they run, the skills and agent
# definitions that steer model runs, the release-notes agent's
# permission config, the dependency-update policy, and who owns review
# approval — so they always warrant the full review. Every one of them
# is docs-classified by facts.py's DOCS_PATH (all .github/ paths; all
# .md files), so the prefix match decides relevance before — and
# independently of — any code-change requirement: round-5's bypass was
# exactly a docs-classified agent config no-opping the lane as docs.
# The old prompt spelled this rule for the model; the script is now the
# one spelling, and the model only runs when it holds.
ALWAYS_RELEVANT_PREFIXES = (
    ".github/workflows/",
    ".github/actions/",
    ".github/scripts/",
    ".github/prompts/",
    ".github/dependabot.yml",
    ".github/CODEOWNERS",
    ".github/opencode-release-notes.json",
    ".agents/skills/",
    ".opencode/agents/",
    ".claude/agents/",
    "AGENTS.md",
    "CLAUDE.md",
    # The shipped skill surfaces: the CLI's bundled skill (its
    # references steer every agent that runs the skill) and the Python
    # wheel's library skill (same, for Python users' agents) — .md
    # files living under surfaces/, so every other classifier calls
    # them docs; they are agent-instruction surfaces, always relevant
    # (the round-27 finding, the same class as the .agents/ entries
    # above).
    "surfaces/relate-cli/src/skill/bundle/",
    "surfaces/relate-python/python/relate/.agents/",
)
RUST_SUFFIXES = (".rs",)
RUST_FILENAMES = ("Cargo.toml", "Cargo.lock")


def _gate_paths(facts: dict) -> list[str]:
    """Every path a changed file occupied in the diff: its HEAD path
    plus, for a rename or copy, the old path facts.py records — its one
    spelling of the rename old name. The relevance predicates judge a
    file on both endpoints: a rename into a docs-classified path is a
    change to the file that was there, not a docs-only diff, so the
    no-op gate can never be dodged by the new name alone."""
    paths = []
    for f in facts["files"]:
        paths.append(f["path"])
        if f.get("old_path"):
            paths.append(f["old_path"])
    return paths


def _rust_relevant(facts: dict) -> bool:
    return any(
        path.endswith(RUST_SUFFIXES) or os.path.basename(path) in RUST_FILENAMES
        for path in _gate_paths(facts)
    )


def _security_relevant(facts: dict) -> bool:
    paths = _gate_paths(facts)
    if any(path.startswith(ALWAYS_RELEVANT_PREFIXES) for path in paths):
        return True
    # facts.py's docs classification (its one spelling) decides "code
    # change" — judged over BOTH rename endpoints, like the prefix
    # match above: a code file renamed into a docs-classified path is
    # a code change to the file that was there, so a code→docs rename
    # alone cannot no-op the lane.
    return any(not stellar_facts.is_docs(path) for path in paths)


@dataclasses.dataclass(frozen=True)
class KindSpec:
    """One review kind's posting surface: the no-op gate's relevance rule
    (`gate` is the negation — nothing kind-relevant changed) and the
    summary texts. Presentation and gate live in one registry entry so
    the two can never drift apart per kind. The kind's name is the
    registry key — the one spelling of the vocabulary — so the entry
    never repeats it as a field; the lane marker derives from the key
    (marker_for), never from a second copy."""

    heading: str
    posture: str
    noop_note: str
    is_relevant: Callable[[dict], bool]


KINDS = {
    "Rust review": KindSpec(
        heading="**Rust review** (rust-reviewer skill, full review)",
        posture="The rust-reviewer skill performed a full review of the pull request diff.",
        noop_note="no rust-relevant changes (no Rust files modified). ✅",
        is_relevant=_rust_relevant,
    ),
    "Security review": KindSpec(
        heading="**Security review** (security-review skill)",
        posture="The security-review skill reviewed the pull request diff for security issues.",
        noop_note="no security-relevant changes. ✅",
        is_relevant=_security_relevant,
    ),
}


def kind_spec(kind: str) -> KindSpec:
    """The kind's registry entry; an unregistered kind is a loud failure,
    never a default."""
    try:
        return KINDS[kind]
    except KeyError:
        raise SystemExit(
            f"opencode-review-post.py: unknown review kind {kind!r} "
            f"(registered: {', '.join(sorted(KINDS))})"
        ) from None


def marker_for(kind: str) -> str:
    """The lane marker `**<kind>:**`, derived once from the registry key.
    It is the load-bearing string of the whole gate: summary discovery
    (find_summary_comment) matches it at line start, and the enforce
    gate's tier 3 parses lines that start with it. Every consumer —
    marker_line, the summary openers, the land_round/land_fixed_round
    calls — consults this one derivation, so the rendered marker lines
    and the discovery key agree by construction."""
    return f"**{kind}:**"


# ---------------------------------------------------------------------------
# findings validation


def findings_field_errors(doc: dict) -> str | None:
    """The per-finding contract: None when every finding is well-formed,
    else the first offending finding's diagnosis."""
    for i, f in enumerate(doc["findings"]):
        if not isinstance(f, dict):
            return f"findings[{i}]: not an object"
        missing = [k for k in REQUIRED_FIELDS if k not in f]
        if missing:
            return f"findings[{i}]: missing {missing}"
        if f["severity"] not in SEVERITIES:
            return f"findings[{i}]: unknown severity {f['severity']!r}"
        if type(f["line"]) is not int or f["line"] < 1:
            return f"findings[{i}]: line is {f['line']!r}, not a positive integer"
        for k in ("path", "category", "title", "why", "fix"):
            if not isinstance(f[k], str):
                return f"findings[{i}]: {k} is {f[k]!r}, not a string"
    return None


def kind_findings_error(doc: object) -> str | None:
    """The kind findings contract, composed once: the shared envelope —
    stellar post.py's findings_doc_shape_error, the one spelling of
    'this is a version-1 findings document', imported not copied — plus
    the per-finding contract above. check (the workflow's gate) and
    post's own load consume this one composition, so the gate and the
    poster can never decide differently on one document."""
    return stellar_post.findings_doc_shape_error(doc) or findings_field_errors(doc)


def sanitize_findings(findings: list[dict]) -> list[dict]:
    """Model text lands in inline comments and the summary; HTML-comment
    markers would let it forge the verdict marker or the stellar ledger
    block. post.py's sanitize is the one boundary — applied here at the
    same trust crossing."""
    return [
        {k: (stellar_post.sanitize(v) if isinstance(v, str) else v) for k, v in f.items()}
        for f in findings
    ]


def load_findings(path: str) -> list[dict]:
    """Read + validate + sanitize a findings document; a shape or field
    violation exits with the diagnosis (the workflow only routes valid
    documents here, so this is the same ruling restated, not a second
    policy)."""
    with open(path) as fh:
        doc = json.load(fh)
    error = kind_findings_error(doc)
    if error is not None:
        raise SystemExit(f"{path}: {error}")
    return sanitize_findings(doc["findings"])


# ---------------------------------------------------------------------------
# verdict derivation and rendering


def verdict_from(findings: list[dict]) -> tuple[str, int]:
    """ADR-0258's severity gate, the one spelling for these lanes: fail
    when any finding is Critical/High/Medium, pass when the worst is Low
    or there are none. The returned count is the marker line's N — the
    fail-severity findings, the number that made the check fail."""
    failing = sum(1 for f in findings if f["severity"] in FAIL_SEVERITIES)
    return ("fail" if failing else "pass"), failing


def marker_line(kind: str, verdict: str, failing: int) -> str:
    """The load-bearing verdict signal (ADR-0258): the enforce gate's
    tier 3 parses this line, summary discovery matches on its marker,
    and its value always agrees with the verdict file."""
    marker = marker_for(kind)
    if verdict == "pass":
        return f"{marker} ✅ no issues found"
    return f"{marker} ⚠️ {failing} issue(s) flagged inline"


def sorted_findings(findings: list[dict]) -> list[dict]:
    """The one presentation order — severity first, then location —
    shared by the inline comments and the summary list."""
    return sorted(findings, key=lambda f: (SEVERITY_RANK[f["severity"]], f["path"], f["line"]))


def inline_body(f: dict) -> str:
    """The protocol's inline-comment body: [Severity] Category, why, fix."""
    return (f"**[{f['severity']}] {f['category']}:** {f['title']}\n\n"
            f"Why this matters: {f['why']}\nSuggested fix: {f['fix']}")


def build_summary(spec: KindSpec, kind: str, findings: list[dict], verdict: str,
                  failing: int) -> str:
    """Render the summary comment: heading, posture line, one bullet per
    finding (or 'No issues found.'), then the marker line and the
    machine-readable verdict comment. `kind` is the registry key the
    caller already holds (argparse's --kind choice) — the marker derives
    from it, never from a repeated spec field. Deterministic — no model
    prose."""
    lines = [spec.heading, "", spec.posture, ""]
    if findings:
        lines.extend(
            f"- **[{f['severity']}] {f['category']}** "
            f"`{f['path']}:{f['line']}` — {f['title']}"
            for f in findings
        )
    else:
        lines.append("No issues found.")
    lines.append("")
    lines.append(marker_line(kind, verdict, failing))
    lines.append(stellar_post.verdict_comment(verdict))
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------
# commands


def cmd_gate(args: argparse.Namespace) -> None:
    # --facts - reads the facts from stdin: the trusted producer (the
    # prep, piping facts.py --stdout-json) hands the decision its bytes
    # directly, so the gate never re-reads the model-writable
    # .stellar/facts.json between a trusted write and this read.
    if args.facts == "-":
        # First JSON value from the stream, tolerantly: the producer may
        # legitimately emit progress text elsewhere in the stream; the
        # facts object is the value that matters (and, in pipe mode, the
        # stream's final content). raw_decode parses from the first
        # object opener, so leading progress lines cannot break it.
        text = sys.stdin.read()
        obj_at = text.find("{")
        if obj_at < 0:
            raise ValueError("gate: no JSON object in the piped facts stream")
        facts, _ = json.JSONDecoder().raw_decode(text, obj_at)
    else:
        with open(args.facts) as fh:
            facts = json.load(fh)
    spec = kind_spec(args.kind)
    print("true" if not spec.is_relevant(facts) else "false")


def cmd_check(args: argparse.Namespace) -> None:
    """The one findings-document gate for these lanes: print 'valid' and
    exit 0, or a distinct diagnosis and exit 1, so a no-findings round
    names its cause instead of a conflated generic message. The
    read-and-diagnose ladder is the stellar core's
    (check_findings_file, consumed by import); the lane's own contract
    is the composed kind ruling (kind_findings_error)."""
    stellar_post.check_findings_file(args.findings, kind_findings_error)


def cmd_post(args: argparse.Namespace) -> None:
    spec = kind_spec(args.kind)
    findings = sorted_findings(load_findings(args.findings))
    verdict, failing = verdict_from(findings)
    comments = [
        {"path": f["path"], "line": f["line"], "side": "RIGHT", "body": inline_body(f)}
        for f in findings
    ]
    review_doc = {
        "commit_id": args.head_sha,
        "event": "COMMENT",
        "body": f"{args.kind}: {len(comments)} inline finding(s); see summary comment.",
        "comments": comments,
    }
    summary = build_summary(spec, args.kind, findings, verdict, failing)
    if args.dry_run:
        stellar_post.dry_run_dump(review_doc, verdict, summary)
        return
    # The review POST first (post_review's degrade policy is the one
    # home — a failed post degrades to summary-only, and both attempts
    # dying re-raises); the summary and verdict land through the one
    # choreography, forced to fail if the summary upsert dies. The kind
    # lanes own no ledger state, so the stellar carry-forward policy is
    # explicitly declined — the carry boundary.
    if comments:
        stellar_post.post_review(args.repo, args.pr, review_doc)
    stellar_post.land_round(args.repo, args.pr, summary, verdict, args.verdict_file,
                            marker=marker_for(args.kind), carry_ledger=False)
    print(f"post: {len(comments)} inline finding(s), verdict {verdict}", file=sys.stderr)


def cmd_noop(args: argparse.Namespace) -> None:
    """Kind-irrelevant diffs: no model run, verdict pass, tidy summary —
    the marker phrase matches the enforce gate's generic no-op tier. The
    landing choreography is the stellar core's land_fixed_round, with
    the kind's marker and no ledger carry."""
    spec = kind_spec(args.kind)
    summary = (f"{marker_for(args.kind)} {spec.noop_note}\n"
               f"{stellar_post.verdict_comment('pass')}\n")
    stellar_post.land_fixed_round(args, summary, "pass",
                                  marker=marker_for(args.kind), carry_ledger=False)


def cmd_fail(args: argparse.Namespace) -> None:
    """Fail-closed (ADR-0258): the model errored, timed out, or produced
    no valid findings document — the check must fail loudly, not
    silently pass. The reason is model-adjacent text (the check
    diagnosis embeds raw model values), so it crosses the sanitize
    boundary at this one site. The landing choreography is the stellar
    core's land_fixed_round, with the kind's marker and no ledger
    carry."""
    summary = (f"{marker_for(args.kind)} ⚠️ review failed to produce valid findings. "
               f"Details: {stellar_post.sanitize(args.reason)}\n"
               f"{stellar_post.verdict_comment('fail')}\n")
    stellar_post.land_fixed_round(args, summary, "fail",
                                  marker=marker_for(args.kind), carry_ledger=False)


# One spec per subcommand — registration and dispatch both index this
# table, so an unknown command is impossible by construction (post.py's
# registration shape).
_COMMON = [
    (("--dry-run",), {"action": "store_true"}),
    (("--verdict-file",), {"default": "./.opencode-verdict.txt"}),
]
_KIND = [
    (("--kind",), {"required": True, "choices": sorted(KINDS)}),
]
_REPO_PR = [
    (("--repo",), {"required": True}),
    (("--pr",), {"required": True}),
]

COMMANDS = {
    "gate": {"handler": cmd_gate, "extra_args": [
        (("--facts",), {"required": True}),
        *_KIND,
    ]},
    "check": {"handler": cmd_check, "extra_args": [
        (("--findings",), {"required": True}),
    ]},
    "noop": {"handler": cmd_noop, "extra_args": [
        *_COMMON,
        *_KIND,
        *_REPO_PR,
    ]},
    "post": {"handler": cmd_post, "extra_args": [
        (("--head-sha",), {"required": True}),
        (("--findings",), {"required": True}),
        *_COMMON,
        *_KIND,
        *_REPO_PR,
    ]},
    "fail": {"handler": cmd_fail, "extra_args": [
        (("--reason",), {"default": "findings.json missing or invalid"}),
        *_COMMON,
        *_KIND,
        *_REPO_PR,
    ]},
}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name, spec in COMMANDS.items():
        p = sub.add_parser(name)
        for flags, kwargs in spec["extra_args"]:
            p.add_argument(*flags, **kwargs)
    args = ap.parse_args()
    COMMANDS[args.cmd]["handler"](args)


if __name__ == "__main__":
    main()
