#!/usr/bin/env python3
"""stellar-review poster (ADR-0271).

Turns the model's findings JSON into GitHub state, deterministically:
rolls the ledger forward, posts all inline comments in ONE
`POST /repos/{repo}/pulls/{pr}/reviews` call, derives the verdict itself
(never the model), and updates the single summary comment — including the
hidden ledger block the next round boots from.

Commands:
    fetch-ledger  Print the ledger JSON embedded in the PR's summary
                  comment (empty ledger when none exists yet).
    post          Apply findings, post review + summary, write verdict.

Usage:
    post.py fetch-ledger --repo owner/name --pr N
    post.py post --repo owner/name --pr N --head-sha SHA \
        --facts facts.json --ledger ledger.json --findings findings.json \
        [--waive ID:reason ...] [--dry-run] [--verdict-file PATH]
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import subprocess
import sys
import tempfile
import zlib
from collections.abc import Callable

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import briefs as briefs_mod  # noqa: E402
import facts as facts_mod  # noqa: E402
import ledger as ledger_mod  # noqa: E402
from fileout import ensure_parent  # noqa: E402
# The summary comment's renderer, the ledger blob it carries, and the
# rendering vocabulary the poster and the renderer share — one module
# (summary.py), imported here so post.py stays the gh client + findings
# policy + commands (REL8-1769). The per-kind posters read verdict_comment
# through this module's namespace (their `import post`).
from summary import (  # noqa: E402
    LEDGER_RE,
    SEVERITY_TO_GH,
    SUMMARY_MARKER,
    build_summary,
    decode_ledger,
    encode_ledger,
    partition,
    verdict_comment,
)

FAIL_SEVERITIES = {"blocker", "medium"}


def is_fail_severity(finding: dict) -> bool:
    """ADR-0354's verdict policy: the poster threads only findings whose
    severity can fail the verdict. The one spelling of the policy, so a
    severity-vocabulary change touches one site."""
    return finding["severity"] in FAIL_SEVERITIES
NIT_CAP = 8

# ---------------------------------------------------------------------------
# gh helpers


def gh(*args: str, input_path: str | None = None) -> str:
    cmd = ["gh", "api", *args]
    if input_path:
        cmd += ["--input", input_path]
    out = subprocess.run(cmd, check=False, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if out.returncode != 0:
        # the API error message is the diagnostic — never swallow it
        sys.stderr.write(out.stderr.decode(errors="replace"))
        raise subprocess.CalledProcessError(out.returncode, cmd,
                                            output=out.stdout, stderr=out.stderr)
    return out.stdout.decode()


def find_summary_comment(repo: str, pr: str, marker: str = SUMMARY_MARKER) -> dict | None:
    """The bot's own summary comment carrying `marker` (the stellar
    marker by default; the per-kind posters pass their kind's — one
    discovery implementation for every review lane). A comment counts
    only when one of its lines starts with the marker — the same
    anchored contract the enforce gate's tier 3 parses — so a marker
    quoted mid-sentence inside model prose (routine when a stellar
    review quotes these very scripts) is not a summary claim and can
    never redirect another lane's upsert."""
    raw = gh(f"repos/{repo}/issues/{pr}/comments", "--paginate")
    comments = json.loads(raw)
    # leading whitespace is trimmed exactly as the gate's tier 3 trims it
    mine = [
        c for c in comments
        if c["user"]["login"] == "github-actions[bot]"
        and any(line.lstrip().startswith(marker)
                for line in c.get("body", "").splitlines())
    ]
    mine.sort(key=lambda c: c["created_at"])
    return mine[-1] if mine else None


def last_ledger(body: str) -> dict | None:
    """Decode the comment body's LAST (authoritative) stellar-ledger
    block. None when absent; undecodable blocks warn loudly (a state
    reset must leave a trace) and also yield None."""
    matches = LEDGER_RE.findall(body or "")
    if not matches:
        return None
    try:
        return decode_ledger(matches[-1])
    except (json.JSONDecodeError, zlib.error, ValueError):
        print("post: warning: stellar-ledger block present but undecodable — "
              "gate state resets to a full round", file=sys.stderr)
        return None


def extract_ledger_from_comment(body: str) -> dict:
    led = last_ledger(body)
    if led is None:
        return ledger_mod.empty_ledger()
    if led.get("version") != ledger_mod.VERSION:
        print(f"post: warning: stellar-ledger version {led.get('version')} != "
              f"{ledger_mod.VERSION} — gate state resets to a full round", file=sys.stderr)
        return ledger_mod.empty_ledger()
    return led


# ---------------------------------------------------------------------------
# findings validation and shaping

# Derived from the production pass registry (briefs.PASS_LAYOUT) — a pass
# added there is a valid finding area here, with no second copy to drift.
FINDING_AREAS = frozenset(briefs_mod.PASS_LAYOUT)


def sanitize(text: str) -> str:
    """Model-written text lands in the summary comment; HTML comment
    markers would let it forge a stellar-ledger block or break the
    verdict marker parsing downstream, and a line break would let it
    open its own line — a lane marker at line start is the summary
    discovery key (find_summary_comment) and the enforce gate's tier-3
    parse, so model text must never be able to start one. Every line
    boundary str.splitlines sees — newline, carriage return, and the
    Unicode separators, the same boundary set discovery splits on — is
    flattened to a space, so a marker survives only mid-line, where
    neither discovery nor the gate matches it. Loops to a fixpoint: a
    single pass can reassemble a marker across the seam (`<!---->`)."""
    while "<!--" in text or "-->" in text:
        text = text.replace("<!--", "").replace("-->", "")
    return " ".join(text.splitlines())


class FindingsError(SystemExit):
    pass


def findings_doc_shape_error(doc: object) -> str | None:
    """The one spelling of 'this is a version-1 findings document'
    (LABS-615/582): None when the shape is valid, else the
    offending-key diagnosis. validate_findings (the poster's per-finding
    validation) and cmd_check (the workflow's gate) both consume it, so
    the gate and the poster can never rule differently on the same
    document. `type(v) is not int` keeps bool/float versions (True == 1,
    1.0 == 1) out — the contract names the integer."""
    if not isinstance(doc, dict):
        return "wrong shape — top level is not an object"
    v = doc.get("version")
    if type(v) is not int or v != 1:
        return f"wrong shape — version is {v!r}, not 1"
    if not isinstance(doc.get("findings"), list):
        return "wrong shape — 'findings' is missing or not a list"
    return None


def validate_findings(doc: dict) -> list[dict]:
    shape = findings_doc_shape_error(doc)
    if shape is not None:
        raise FindingsError(shape)
    out = []
    for i, f in enumerate(doc["findings"]):
        missing = [k for k in ("area", "severity", "path", "start_line", "quote", "why", "remedy") if k not in f]
        if missing:
            raise FindingsError(f"findings[{i}]: missing {missing}")
        if f["area"] not in FINDING_AREAS:
            raise FindingsError(f"findings[{i}]: unknown area {f['area']!r}")
        if f["severity"] not in SEVERITY_TO_GH:
            raise FindingsError(f"findings[{i}]: unknown severity {f['severity']!r}")
        f = {k: (sanitize(v) if isinstance(v, str) else v) for k, v in f.items()}
        out.append(f)
    return out


def dedupe(findings: list[dict]) -> list[dict]:
    """Same file + overlapping lines + same checklist id = one finding."""
    rank = {"blocker": 0, "medium": 1, "nit": 2}
    kept: list[dict] = []
    for f in sorted(findings, key=lambda x: rank[x["severity"]]):
        dupe = False
        for k in kept:
            if k["path"] != f["path"] or k["checklist_id"] != f["checklist_id"]:
                continue
            a = finding_span(k)
            b = finding_span(f)
            if a[0] <= b[1] and b[0] <= a[1]:
                dupe = True
                break
        if not dupe:
            kept.append(f)
    return kept


ONE_K_CROSSING = "flag-1k-crossing"


def cap_nits(findings: list[dict]) -> tuple[list[dict], int]:
    """Only high-conviction nits survive, capped (merge-time rationing,
    ADR-0271). Returns (kept, dropped_count)."""
    nits, keep = partition(findings, lambda f: not is_fail_severity(f))
    strong = [f for f in nits if f.get("confidence", "high") == "high"]
    kept_nits = strong[:NIT_CAP]
    dropped = len(findings) - len(keep) - len(kept_nits)
    return keep + kept_nits, dropped


def anchor_line(f: dict, hunks_by_path: dict[str, list[list[int]]]) -> int | None:
    """The diff line an inline comment anchors to, or `None` when the
    finding has no anchor in the diff. GitHub rejects the whole review
    (HTTP 422, `could not be resolved`) when any one comment's `line` is
    not part of the diff at `commit_id` — and `intersects_hunks` only
    proves the finding's *range* overlaps a hunk, while the anchor is the
    range's end line, which can sit past the hunk's last changed line
    (observed on PR #390 round 30: a 587-593 finding against a 580-592
    hunk anchored at 593 and lost the whole four-comment batch). The
    anchor clamps into the intersecting hunks; a finding whose range
    misses every hunk — the 1k-crossing exemption admits those — has **no**
    inline anchor, and returning its end line unclamped is exactly the 422
    above, so it resolves to `None` and the caller threads it through the
    summary instead."""
    lo, hi = finding_span(f)
    ends = facts_mod.intersecting_hunk_ends(f["path"], lo, hi, hunks_by_path)
    # The hunk containing `end` wins — min(hi, e) is `hi` for it and below
    # `hi` for every hunk under it, so the maximum is the clamp.
    return max((min(hi, e) for e in ends), default=None)


def intersects_hunks(f: dict, hunks_by_path: dict[str, list[list[int]]]) -> bool:
    """The one on-diff policy (ADR-0275, ADR-0346): a finding is on the
    diff when its whole line *range* intersects a hunk — flag-1k-crossing
    is exempt because file growth is a whole-file fact whose natural quote
    (the file span) never intersects a hunk."""
    if f.get("checklist_id") == ONE_K_CROSSING:
        return True
    lo, hi = finding_span(f)
    return facts_mod.range_in_hunks(f["path"], lo, hi, hunks_by_path)


# ---------------------------------------------------------------------------
# scope enforcement (ADR-0275) — "sealed means sealed" is a script
# invariant here, not a prompt instruction entrusted to the model


def filter_scope(findings: list[dict], scope_doc: dict) -> tuple[list[dict], list[dict]]:
    """Drop findings outside the round's assigned scope: a finding with a
    `unit` must name a review unit; a unit-less (file-level) finding must
    name a review file. Dropped findings never enter the ledger."""
    review_units = set(scope_doc.get("review_units", []))
    review_files = set(scope_doc.get("review_files", [])) | set(
        scope_doc.get("dependency_dirty_files", []))
    def in_scope(f):
        return f["unit"] in review_units if f.get("unit") else f["path"] in review_files

    return partition(findings, in_scope)


def incremental_hunks(diff_text: str) -> dict[str, list[list[int]]]:
    """path → new-file hunk ranges for a multi-file patch (the
    last_head…head incremental diff). A section that yields no hunks
    warns loudly — folding a parse miss into the generic out-of-scope
    drop list would silently shrink the round's anchor set."""
    out = {}
    for path, lines in briefs_mod.diff_sections(diff_text):
        _, ranges = facts_mod.parse_file_diff("\n".join(lines))
        if not ranges:
            print(f"post: warning: no hunks parsed for diff section {path}", file=sys.stderr)
        out[path] = ranges
    return out


def filter_incremental(findings: list[dict],
                       hunks_by_path: dict[str, list[list[int]]]) -> tuple[list[dict], list[dict]]:
    """Round ≥ 2: a finding's line range must intersect a hunk of the
    incremental diff — the push is judged on the lines it changed, not on
    the whole re-scoped unit around them (ADR-0275). The on-diff test and
    its exemptions are `intersects_hunks`'s own policy."""
    return partition(findings, lambda f: intersects_hunks(f, hunks_by_path))


def finding_span(f: dict) -> tuple[int, int]:
    """A finding's line span — `start_line`, with the optional `end_line`
    resolved to it. The one statement of what a finding's range is; the
    anchoring, incremental-filter and dedupe paths all read it."""
    return f["start_line"], f.get("end_line") or f["start_line"]


def finding_doc(entry: dict) -> dict:
    """Ledger entry → finding-doc shape (the one adapter — anchoring and
    comment rendering both consume this)."""
    end = entry["lines"][1] or entry["lines"][0]
    return {"path": entry["path"], "start_line": entry["lines"][0], "end_line": end,
            "severity": entry["severity"], "area": entry["area"],
            "checklist_id": entry["checklist_id"], "why": entry["summary"],
            "remedy": entry.get("remedy", "")}


def inline_body(f: dict, fid: str) -> str:
    sev = SEVERITY_TO_GH[f["severity"]]
    return (f"**[{sev}] {f['area']} ({f['checklist_id']}):** {f['why']}\n\n"
            f"Suggested fix: {f['remedy']}\n\n_finding {fid}_")


# ---------------------------------------------------------------------------
# post command


def dry_run_dump(review_doc: dict, verdict: str, summary: str) -> None:
    """The one dry-run dump for every posting command — stellar's post
    and the per-kind lanes' (consumed by import): the review payload,
    the verdict, the summary. Printed, nothing landed."""
    print(json.dumps(review_doc, indent=2, sort_keys=True))
    print("--- verdict:", verdict)
    print(summary)


def inline_batch(fail_new: list[dict],
                 hunks: dict[str, list[list[int]]]) -> tuple[list[dict], set[str]]:
    """Attempt one inline comment per fail-severity finding (the ADR-0354
    split's threading half — context findings ride the summary once, nits
    the collapsed appendix). Returns the comment payloads and the set of
    finding ids it attempted (every finding it built a comment for). A
    finding whose range misses every hunk (the 1k-crossing exemption's
    whole-file findings) has no line to anchor to — an off-diff line is the
    422 that loses the batch — so it is not attempted and rides the
    summary's new-findings section instead. cmd_post turns the attempted
    set into the rejected set once, after the post."""
    comments: list[dict] = []
    attempted: set[str] = set()
    for f in fail_new:
        fdoc = finding_doc(f)
        line = anchor_line(fdoc, hunks)
        if line is None:
            print(f"post: {f['id']} has no diff anchor; summary only",
                  file=sys.stderr)
            continue
        attempted.add(f["id"])
        comments.append({
            "path": fdoc["path"],
            "line": line,
            "side": "RIGHT",
            "body": inline_body(fdoc, f["id"]),
        })
    return comments, attempted


def cmd_post(args: argparse.Namespace) -> None:
    with open(args.facts) as fh:
        facts = json.load(fh)
    led = ledger_mod.load_ledger(args.ledger)
    with open(args.findings) as fh:
        doc = json.load(fh)
    findings = dedupe(validate_findings(doc))
    findings, dropped_nits = cap_nits(findings)

    round_no = led["rounds"] + 1
    # Resolutions are honored only for ids this round's scope actually
    # checked — never for findings on sealed units (trust boundary: the
    # findings document is model output). Fail closed: no scope document,
    # no resolutions.
    allowed: set[str] = set()
    scope_stats = None
    dropped_scope: list[dict] = []
    if args.scope:
        with open(args.scope) as fh:  # missing file must raise, loudly
            scope_doc = json.load(fh)
        scope_stats = scope_doc["stats"]
        allowed = {rc["finding_id"] for rc in scope_doc["resolve_checks"]}
        # ADR-0275: findings get the same scope enforcement resolutions
        # already had — sealed units and out-of-scope files cannot
        # produce findings, whatever the model emitted.
        findings, dropped_scope = filter_scope(findings, scope_doc)
        # Incremental rounds judge the push's changed lines only; a
        # finding on unchanged lines of a re-scoped unit is dropped.
        # `full` is indexed directly: a malformed scope document fails
        # loudly here, matching the surrounding fail-closed reads.
        if not scope_doc["full"] and args.diff:
            with open(args.diff) as fh:
                findings, dropped_incremental = filter_incremental(
                    findings, incremental_hunks(fh.read()))
            dropped_scope += dropped_incremental
    for f in dropped_scope:
        print(f"post: dropped out-of-scope finding on {f['path']}:{f['start_line']} "
              f"({f.get('checklist_id', '')})", file=sys.stderr)
    doc = dict(doc, findings=findings)
    led = ledger_mod.apply_update(facts, led, doc, args.waive, allowed_resolve_ids=allowed,
                                  resolves=args.resolve)
    # ledger entries are the single source of truth from here on; the
    # disposition query lives in ledger.py, the module that owns the
    # entry schema
    disp = ledger_mod.round_dispositions(led, round_no)
    new_entries = disp["new"]

    # ADR-0346: split new findings once — diff-anchored vs context — and
    # every consumer (verdict, inline comments, summary) reads the same
    # split. A finding that does not intersect the PR's diff hunks is
    # pre-existing file debt: ledger + night-shift backlog, printed once
    # in the summary's context section, never a merge blocker here.
    hunks = facts_mod.hunks_by_path(facts)

    def is_anchored(entry):
        return intersects_hunks(finding_doc(entry), hunks)

    anchored_new, unanchored = partition(new_entries, is_anchored)
    anchored_carried = [c for c in disp["carried"] if is_anchored(c)]
    # ADR-0354: the one severity split of the anchored new findings,
    # computed once and shipped in the disposition — fail-severity
    # (blocker/medium) thread inline; nits never do, they ride the
    # collapsed appendix.
    fail_new, low_new = partition(anchored_new, is_fail_severity)

    # gate: any open blocker/medium on this PR's own diff fails (ADR-0258
    # severities via mapping) — anchored new + anchored carried only
    # ADR-0354: anchored new fail-severity findings arrive pre-split
    # (fail_new); the severity membership applies once more, to the
    # anchored carried — one spelling of the verdict's failing set
    failing = fail_new + [c for c in anchored_carried if is_fail_severity(c)]
    # The timeout salvage path (LABS-635) folds into the one verdict
    # expression: a partial round's findings still count (ledger, inline,
    # summary) so the killed round's work is not lost — but a partial
    # review can never green the gate: the uncovered passes might have
    # found blockers.
    verdict = "fail" if failing or args.timeout_partial else "pass"

    # inline comments: the fail-severity half of the ADR-0354 split above;
    # context findings ride in the summary deterministically (once —
    # ADR-0346), and nits ride the collapsed Low appendix. The rejected
    # set is minted once, after the post: what was attempted, if the batch
    # was refused, else nothing.
    comments, attempted = inline_batch(fail_new, hunks)

    review_doc = {
        "commit_id": args.head_sha,
        "event": "COMMENT",
        "body": f"Stellar review round {round_no}: "
                f"{len(comments)} inline finding(s); see summary comment.",
        "comments": comments,
    }

    # attempt the review post first, then build the summary exactly once
    # from the final disposition; the degraded re-bucketing lives in
    # build_summary, not here, and the degrade-vs-raise policy is
    # post_review's alone (LABS-615) — a raise here means every posting
    # path is down and the step fails loudly with the run's artifacts
    degraded = False
    if comments and not args.dry_run:
        degraded = post_review(args.repo, args.pr, review_doc)
    summary = build_summary(facts, scope_stats, round_no, args.head_sha, verdict, led,
                            {**disp, "fail_new": fail_new, "low_new": low_new,
                             "unanchored": unanchored,
                             "anchored_carried": anchored_carried,
                             "dropped_nits": dropped_nits,
                             "dropped_scope": len(dropped_scope),
                             "rejected": attempted if degraded else set(),
                             "timeout_partial": args.timeout_partial})

    if args.dry_run:
        dry_run_dump(review_doc, verdict, summary)
        return

    # LABS-615: the posting choreography (dead-path dump, forced-fail
    # verdict, re-raise) is land_round's — one home for every
    # summary-carrying command
    land_round(args.repo, args.pr, summary, verdict, args.verdict_file)
    print(f"post: round {round_no}, {len(comments)} inline, verdict {verdict}", file=sys.stderr)


def tempfile_payload(doc: dict) -> str:
    fd, path = tempfile.mkstemp(suffix=".json")
    with os.fdopen(fd, "w") as fh:
        json.dump(doc, fh)
    return path


def upsert_summary(repo: str, pr: str, body: str, marker: str = SUMMARY_MARKER,
                   carry_ledger: bool = True) -> None:
    """The one summary-comment choreography: discover-then-edit, so the PR
    keeps exactly one summary per review lane (the stellar marker by
    default; the per-kind posters pass their kind's). `carry_ledger` is
    the explicit stellar-lane boundary: stellar's commands keep the
    default — when the new body carries no ledger block of its own
    (noop/fail summaries), the old block is carried forward, because
    dropping it would wipe sealed-unit state and human waivers — while
    the per-kind lanes decline it: their comments own no ledger state,
    so no stellar policy runs on their path."""
    existing = find_summary_comment(repo, pr, marker)
    if carry_ledger and existing and not LEDGER_RE.search(body):
        # re-encode to the canonical z: form, never verbatim
        decoded = last_ledger(existing["body"])
        if decoded is not None:
            body += f"<!-- stellar-ledger: {encode_ledger(decoded)} -->\n"
    with payload_file({"body": body}) as p:
        if existing:
            try:
                gh(f"repos/{repo}/issues/comments/{existing['id']}", "-X", "PATCH", input_path=p)
            except subprocess.CalledProcessError:
                # LABS-615: the edit failed — create a fresh comment
                # instead (a rare duplicate beats a lost round; the next
                # round's discover picks the newest, and the enforce
                # gate accepts any marker-carrying comment). Scoped to
                # the PATCH arm: a failed initial create is not a
                # fallback case — it propagates to the caller's
                # dead-path handling rather than blind-retrying the
                # same POST (which could double-post).
                print("post: summary PATCH failed; retrying as a fresh comment",
                      file=sys.stderr)
                gh(f"repos/{repo}/issues/{pr}/comments", "-X", "POST", input_path=p)
        else:
            gh(f"repos/{repo}/issues/{pr}/comments", "-X", "POST", input_path=p)


@contextlib.contextmanager
def payload_file(doc: dict):
    """One POST payload file's lifecycle: written on entry, removed on
    exit, exactly once."""
    path = tempfile_payload(doc)
    try:
        yield path
    finally:
        os.remove(path)


def post_review(repo: str, pr: str, review_doc: dict) -> bool:
    """POST the review with inline comments; the ONE degrade-policy home
    (LABS-615): any failed attempt — payload rejection (gh's `(HTTP
    422)` status token) or a transient network/auth/5xx failure —
    degrades to a summary-only retry, because a posting hiccup must not
    cost the round its findings; the failure is named in the run log.
    Raises only when the degraded retry also fails — the fully-dead
    posting path is cmd_post's to fail loudly with the summary attached.
    Returns True when the post was degraded."""
    try:
        with payload_file(review_doc) as p:
            gh(f"repos/{repo}/pulls/{pr}/reviews", "-X", "POST", input_path=p)
        return False
    except subprocess.CalledProcessError as e:
        err = (e.stderr or b"").decode(errors="replace").strip()
        print(f"post: review POST failed ({err or 'no stderr'}); "
              "degrading to summary-only", file=sys.stderr)
        degraded_doc = dict(review_doc, comments=[],
                            body=review_doc["body"] + " (inline comments degraded to summary — see error above)")
        with payload_file(degraded_doc) as p:
            gh(f"repos/{repo}/pulls/{pr}/reviews", "-X", "POST", input_path=p)
        return True


def write_verdict(path: str, verdict: str) -> None:
    with open(path, "w") as fh:
        fh.write(verdict + "\n")


def land_round(repo: str, pr: str, summary: str, verdict: str,
               verdict_file: str, marker: str = SUMMARY_MARKER,
               carry_ledger: bool = True) -> None:
    """The one posting choreography for every summary-carrying command
    (LABS-615), shared by the stellar commands and the per-kind posters:
    `marker` selects which lane's summary is discovered and
    `carry_ledger` is that lane's ledger-carry boundary (upsert_summary's
    docstring). Upsert the summary, then write the verdict. A dead upsert
    must not cost the round its findings OR green the gate — the built
    summary goes to the run log, the verdict is forced to fail (tier 1
    of the enforce gate would otherwise trust the verdict file alone),
    and the error re-raises so the step stays red."""
    try:
        upsert_summary(repo, pr, summary, marker, carry_ledger)
    except subprocess.CalledProcessError:
        print("post: summary upsert failed; round summary follows "
              "(verdict forced to fail)", file=sys.stderr)
        print(summary, file=sys.stderr)
        write_verdict(verdict_file, "fail")
        raise
    write_verdict(verdict_file, verdict)


def land_fixed_round(args: argparse.Namespace, summary: str, verdict: str,
                     marker: str = SUMMARY_MARKER, carry_ledger: bool = True) -> None:
    """The fixed-verdict commands' (noop, fail) full choreography, shared
    by the stellar commands and the per-kind posters (`marker` selects
    the lane, `carry_ledger` its ledger-carry boundary): the dry-run
    escape — print the summary, land nothing — has exactly one spelling,
    and the posting path is land_round's."""
    if args.dry_run:
        print(summary)
        return
    land_round(args.repo, args.pr, summary, verdict, args.verdict_file,
               marker, carry_ledger)


def cmd_noop(args: argparse.Namespace) -> None:
    """Docs/CI-only PRs: no model run at all, verdict pass, tidy summary."""
    summary = (f"{SUMMARY_MARKER} no review-relevant changes (docs/CI-only diff). ✅\n"
               f"{verdict_comment('pass')}\n")
    land_fixed_round(args, summary, "pass")


def check_findings_file(path: str, validate: Callable[[object], str | None]) -> None:
    """The shared read-and-diagnose entry point for every findings gate —
    stellar's check and the per-kind lanes' (consumed by import): print
    'valid' and exit 0 when the file parses and passes `validate` (the
    lane's own document contract — stellar's findings_doc_shape_error,
    the per-kind lanes' composed kind ruling); otherwise print the
    distinct diagnosis (missing / empty / unparseable / the validator's
    ruling) and exit 1, so a no-findings round names its cause instead
    of a conflated generic message. One spelling of the failure
    classes: the two lanes can never name them differently."""
    def reject(detail: str) -> None:
        print(f"{path}: {detail}", file=sys.stderr)
        raise SystemExit(1)

    try:
        with open(path, "rb") as fh:
            raw = fh.read()
    except FileNotFoundError:
        reject("missing — the model run wrote no file")
    if not raw.strip():
        reject("empty — the file exists but holds no bytes")
    try:
        doc = json.loads(raw)
    except UnicodeDecodeError:
        reject("unparseable — not UTF-8 text (a half-flushed document)")
    except json.JSONDecodeError as e:
        reject(f"unparseable JSON — {e.msg} at line {e.lineno} column {e.colno}")
    error = validate(doc)
    if error is not None:
        reject(error)
    print("valid")


def cmd_check(args: argparse.Namespace) -> None:
    """The one findings-document gate (LABS-615, LABS-582): 'valid' +
    exit 0, or a distinct diagnosis + exit 1. The read-and-diagnose
    ladder is check_findings_file's (shared with the per-kind lanes);
    stellar's own document contract is the shape ruling,
    findings_doc_shape_error."""
    check_findings_file(args.findings, findings_doc_shape_error)


def cmd_fail(args: argparse.Namespace) -> None:
    """Fail-closed (ADR-0258): the model errored or produced no valid
    findings.json — the check must fail loudly, not silently pass."""
    # the reason is model-controlled by construction (the workflow
    # builds it from cmd_check's diagnosis, which embeds raw model
    # values) — sanitize() at this one boundary closes the forged
    # stellar-ledger/verdict-marker path (an unsanitized marker would
    # suppress the real ledger's carry-forward in upsert_summary)
    summary = (f"{SUMMARY_MARKER} ⚠️ review failed to produce valid findings. "
               f"Details: {sanitize(args.reason)}\n"
               f"{verdict_comment('fail')}\n")
    land_fixed_round(args, summary, "fail")


def cmd_fetch_ledger(args: argparse.Namespace) -> None:
    existing = find_summary_comment(args.repo, args.pr)
    led = extract_ledger_from_comment(existing["body"]) if existing else ledger_mod.empty_ledger()
    out = json.dumps(led, indent=1, sort_keys=True) + "\n"
    if args.out:
        with open(ensure_parent(args.out), "w") as fh:
            fh.write(out)
    else:
        sys.stdout.write(out)


# One spec per subcommand — registration and dispatch both index this
# table, so an unknown command is impossible by construction.
_COMMON = [
    (("--dry-run",), {"action": "store_true"}),
    (("--verdict-file",), {"default": "./.opencode-verdict.txt"}),
]
# Every summary-carrying command names its PR; check (a pure file gate)
# is the one exception, spelled by omitting the splice.
_REPO_PR = [
    (("--repo",), {"required": True}),
    (("--pr",), {"required": True}),
]

COMMANDS = {
    "fetch-ledger": {"handler": cmd_fetch_ledger, "extra_args": [
        (("--out",), {}),
        *_REPO_PR,
    ]},
    "check": {"handler": cmd_check, "extra_args": [
        (("--findings",), {"required": True}),
    ]},
    "noop": {"handler": cmd_noop, "extra_args": [*_COMMON, *_REPO_PR]},
    "fail": {"handler": cmd_fail, "extra_args": [
        (("--reason",), {"default": "findings.json missing or invalid"}),
        *_COMMON,
        *_REPO_PR,
    ]},
    "post": {"handler": cmd_post, "extra_args": [
        (("--head-sha",), {"required": True}),
        (("--facts",), {"required": True}),
        (("--ledger",), {"required": True}),
        (("--findings",), {"required": True}),
        (("--scope",), {"help": "scope.json for the stats line"}),
        (("--diff",), {"help": "incremental last_head…head patch; in incremental "
                               "rounds, findings not intersecting its hunks are "
                               "dropped (ADR-0275)"}),
        (("--resolve",), {"action": "append", "default": [],
                          "help": "finding-id:note; human escape hatch — marks an open "
                                  "finding resolved when the staleness rules cannot "
                                  "see the fix"}),
        (("--waive",), {"action": "append", "default": []}),
        (("--timeout-partial",), {"action": "store_true",
                                  "help": "salvage mode (LABS-635): the findings "
                                          "document is the partial state of a "
                                          "timed-out run — post the findings, "
                                          "force the verdict to fail"}),
        *_COMMON,
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
