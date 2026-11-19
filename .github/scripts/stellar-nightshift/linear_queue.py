#!/usr/bin/env python3
"""Linear-backed review-task queue (LABS-311, ADR-0292).

The durable queue is Linear issues carrying the `review-tasks` label.
Humans can file such issues by hand; the night-shift job creates one per
finding-cluster and pulls the open ones back.

    linear_queue.py sync --queue queue.json [--team-key LABS] [--label review-tasks]
                         [--out issues.json] [--dry-run]
    linear_queue.py pull [--label review-tasks] [--out tasks.json]
    linear_queue.py backfill [--apply] [--label review-tasks] [--out report.json]

sync is idempotent: the title carries `[stellar-ns:<key>]`; an open issue
with that marker is reused, never duplicated. The label is created on the
team when missing. pull lists every open labelled issue (marker-bearing or
hand-filed) with its parsed cluster `key` (null for hand-filed ones).
Env: LINEAR_API_KEY (not needed with --dry-run).
"""

from __future__ import annotations

import argparse
import functools
import hashlib
import json
import os
import re
import sys
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import nightshift  # noqa: E402

API = "https://api.linear.app/graphql"
OPEN = '{type:{nin:["completed","canceled"]}}'
Q_TEAM = "query($k:String!){teams(filter:{key:{eq:$k}}){nodes{id}}}"
Q_LABEL = "query($t:ID!,$n:String!){issueLabels(filter:{team:{id:{eq:$t}},name:{eq:$n}}){nodes{id}}}"
M_LABEL = "mutation($i:IssueLabelCreateInput!){issueLabelCreate(input:$i){issueLabel{id}}}"
Q_FIND = "query($t:String!){issues(first:1,filter:{title:{contains:$t},state:{type:{neq:\"completed\"}}}){nodes{id identifier url}}}"  # canceled = declined: never re-file
Q_PULL = ("query($n:String!){issues(first:100,filter:{labels:{name:{eq:$n}},state:%s})"
          "{nodes{id identifier title url description}}}") % OPEN
M_CREATE = "mutation($i:IssueCreateInput!){issueCreate(input:$i){success issue{id identifier url}}}"
M_UPDATE = "mutation($id:String!,$i:IssueUpdateInput!){issueUpdate(id:$id,input:$i){success}}"
MARK_RE = re.compile(r"\[stellar-ns:([0-9a-f]{10})\]")


def marker(cluster: dict) -> str:
    return f"[stellar-ns:{cluster['key']}]"


def issue_input(cluster: dict, team_id: str, label_id: str | None) -> dict:
    lines = [f"- **{f['severity']}** `{f['path']}` ({f['id']}): {f['summary']}\n  Remedy: {f['remedy']}"
             for f in cluster["findings"]]
    body = (f"Night-shift cluster `{cluster['crate']}` — {len(cluster['findings'])} finding(s), "
            f"test tier `{cluster['test_tier']}`.\n\n" + "\n".join(lines) +
            "\n\nChecks: " + "; ".join(f"`{c}`" for c in cluster["test_commands"]))
    inp = {"teamId": team_id, "title": f"{marker(cluster)} stellar findings in {cluster['crate']}",
           "description": body, "priority": 3 if cluster["top_severity_rank"] else 2}
    if label_id:
        inp["labelIds"] = [label_id]
    return inp


def gql(query: str, variables: dict, key: str) -> dict:
    req = urllib.request.Request(API, json.dumps({"query": query, "variables": variables}).encode(),
                                 {"Content-Type": "application/json", "Authorization": key})
    with urllib.request.urlopen(req, timeout=30) as r:
        out = json.load(r)
    if out.get("errors"):
        raise RuntimeError(out["errors"])
    return out["data"]


def resolve(team_key: str, label: str, call) -> tuple[str, str]:
    """Team id and label id, creating the label on the team if missing."""
    teams = call(Q_TEAM, {"k": team_key})["teams"]["nodes"]
    if not teams:
        raise RuntimeError(f"no Linear team with key {team_key}")
    tid = teams[0]["id"]
    labels = call(Q_LABEL, {"t": tid, "n": label})["issueLabels"]["nodes"]
    if labels:
        return tid, labels[0]["id"]
    made = call(M_LABEL, {"i": {"teamId": tid, "name": label}})
    return tid, made["issueLabelCreate"]["issueLabel"]["id"]


def materialize(queue: dict, team_id: str, label_id: str | None, call) -> list[dict]:
    """`call(query, variables)` is the transport (injected for tests)."""
    res = []
    for c in queue["runnable"] + queue.get("deferred_outside_window", []):
        nodes = call(Q_FIND, {"t": marker(c)})["issues"]["nodes"]
        if nodes:
            res.append({"key": c["key"], "created": False, **nodes[0]})
            continue
        made = call(M_CREATE, {"i": issue_input(c, team_id, label_id)})["issueCreate"]
        res.append({"key": c["key"], "created": True, **made["issue"]})
    return res


def pull(label: str, call) -> list[dict]:
    """The durable tasks.json document — marker keys AND the shaped
    per-crate clusters, attached here so the contract is what pull
    returns, not CLI glue in main. Explicit-scope tickets (a Files:
    section) sort ahead of derived-scope ones (LABS-684): the fetch
    itself carries the priority, so plan's runnable list and the
    workflow's first-trusted-key pick see the explicit clusters first."""
    out = []
    for n in call(Q_PULL, {"n": label})["issues"]["nodes"]:
        m = MARK_RE.search(n["title"])
        # the external field is normalized once, where it enters: every
        # downstream scan indexes the description directly
        t = {**n, "key": m.group(1) if m else None, "description": n["description"] or ""}
        t["clusters"] = task_clusters(t)
        # the full vocabulary is named — an unknown scope fails loudly
        # here, never silently runs or silently skips (round 3, F-1-4/7)
        t["skipped"] = SKIP_REASONS[t["scope"]]
        out.append(t)
    out.sort(key=lambda t: t["scope"] != "explicit")
    return out


FILES_HEAD_RE = re.compile(r"^\s*(?:#+\s*|\*\*)?files\*{0,2}\s*:?\*{0,2}\s*:?\s*$", re.I)
# the mention pattern is compiled from nightshift's one path-shape body —
# the scanner and the acceptance gate share a single vocabulary source
PATH_RE = re.compile(r"`?(" + nightshift.SAFE_PATH_BODY + ")`?")
UNESCAPE_RE = re.compile(r"\\([_*`\[\]()#.\-])")
# A URL is a link, never a scope file — stripped WHOLE, by construction:
# scheme-prefixed tokens, and any token carrying a host-shaped head
# (`github.com/…`, `linear.app/…`, `raw.githubusercontent.com/…` — a
# dotted label run before the first slash; no workspace path begins
# that way). No URL family (pull/, issues/, blob/, commit/, any host)
# can leak its path-shaped tail into the vocabulary (observed on the
# real backlog: LABS-621/554/430 carried `relate/pull/530`-shaped
# matches)
URL_RE = re.compile(r"(?:https?://|www\.)\S+|(?:[A-Za-z0-9-]+\.)+[A-Za-z]{2,}/\S+")
# no workspace path legitimately ends in sentence punctuation; an
# unquoted mention at a sentence's end carries its period into the
# PATH_RE match
TRAILING_PUNCT = ".,;:!?"


def _unescaped(description: str) -> str:
    """Linear escapes markdown punctuation (`exec\_ops.rs`); the unescape
    is one policy, stated once, consulted by every path scan."""
    return UNESCAPE_RE.sub(r"\1", description or "")


def _scan_paths(text: str) -> list[str]:
    """The ONE path-extraction pipeline (LABS-684): unescape Linear's
    markdown, strip URLs whole, scan the workspace-path shape, trim
    sentence punctuation off an unquoted mention's tail, and keep only
    what passes nightshift's shape/safety rules. Both consumers — the
    Files: section walk and the whole-description fallback — go through
    this, so the same malformed input can never be accepted at one site
    and rejected at the other."""
    text = URL_RE.sub(" ", _unescaped(text))
    # the trimmed match is bound once — the safety check and the result
    # set read the same value, never two re-derivations of the trim
    return sorted({p for m in PATH_RE.finditer(text)
                   if nightshift.safe_path(p := m.group(1).rstrip(TRAILING_PUNCT))})


def parse_files(description: str) -> list[str]:
    """Paths from a `Files:` section (heading line, then content lines
    until a blank line or the next heading — `- path` bullets as
    backfill writes them, or a bare path), through the one path
    pipeline."""
    lines: list[str] = []
    in_sec = False
    for ln in (description or "").splitlines():
        if not in_sec:
            in_sec = bool(FILES_HEAD_RE.match(ln))
            continue
        if not ln.strip() or ln.lstrip().startswith("#"):
            if lines:
                break
            continue
        lines.append(ln)
    return _scan_paths("\n".join(lines))


def has_files_section(description: str) -> bool:
    """Whether the description carries a `Files:` heading at all —
    section PRESENCE, distinct from what the section parses to (it may
    name only non-workspace paths). The derive fallback keys on
    absence, never on an empty parse: a human's explicit-but-odd
    section is honored as their answer, never overridden by prose
    mentions (LABS-684 round 2)."""
    return any(FILES_HEAD_RE.match(ln) for ln in (description or "").splitlines())


def ticket_scope(description: str) -> tuple[list[str], str]:
    """The ONE scope policy for a review-tasks ticket (LABS-684): the
    explicit `Files:` section first — a PRESENT section naming no
    workspace path stays 'empty-section' (the human's explicit answer,
    never overridden, and backfill never appends a second section);
    without a section, the ticket's own workspace-path mentions
    ('derived'); 'none' when neither names a file. task_clusters'
    shaping, backfill's report/append, and pull's explicit-first
    ordering all read this — the fallback ladder is never re-spelled
    at a use site."""
    files = parse_files(description)
    if files:
        return files, "explicit"
    if has_files_section(description):
        return [], "empty-section"
    # the fallback scans the whole description through the one pipeline
    files = _scan_paths(description)
    if files:
        return files, "derived"
    return [], "none"


TASK_CLUSTER_CAP = 8
# the per-cluster file cap, mirroring nightshift's --max-files default —
# named so the two cap policies are distinguishable at a glance
FILES_PER_CLUSTER = 8


def _task_findings(task: dict, files: list[str]) -> list[dict]:
    """One issue -> one finding per file: the input stream cluster() shapes."""
    # the scope source (Files: section or derived mentions) returned
    # files, so the description is present and non-empty — index it,
    # never a silent "" fallback
    remedy = task["description"][:1500]
    return [{"id": task["identifier"], "severity": "medium", "path": p,
             "checklist_id": "task", "quote_hash": task["identifier"],
             "summary": task["title"], "remedy": remedy} for p in files]


# the scope vocabulary's human-readable skip prose, composed at the one
# site that emits tasks.json (pull) — one classification channel, the
# reason strings never travel beside the facts they restate
# the FULL scope vocabulary, one row per value — the runnable members
# (explicit, derived) map to "" and every skip states its reason; pull
# indexes directly, so a scope value without a row fails loudly
SKIP_REASONS = {
    "auto-filed": "auto-filed cluster",
    "explicit": "",
    "empty-section": "Files: section names no workspace path",
    "oversized": f"more than {TASK_CLUSTER_CAP} clusters; split the issue",
    "derived": "",
    "none": "no Files: section and no derivable path",
}
# backfill's report actions, one row per non-writing scope value —
# mirroring SKIP_REASONS, indexed directly so an unknown scope fails
# loudly; 'derived' is the one writing arm and handled in the flow
ACTIONS = {
    "auto-filed": "skip-auto-filed",
    "explicit": "has-section",
    "empty-section": "has-section",
    "oversized": "has-section",
    "none": "no-derivable-path",
}


def task_clusters(task: dict) -> list[dict]:
    """A hand-filed review-tasks issue is an INPUT STREAM, not a 1:1
    execution instruction (2026-10-18: the "P3 findings on merged main"
    issue's 5 files across 4 crates rode one model session and predictably
    failed its tier). The issue's scope — its `Files:` section, or, as
    the LABS-684 fallback, the workspace paths its own items mention —
    becomes per-file findings, shaped by the same per-crate cluster()
    split + file cap as ledger findings: one issue → up to
    TASK_CLUSTER_CAP small clusters, one per crate (the file cap can
    split one crate into siblings). Keys are identifier + crate + sorted
    file set — content-derived and injective over this function's own
    output, so sibling clusters never share one and re-runs dedupe,
    never duplicate. Returns the clusters and records the ONE scope
    resolution on the ticket dict (`scope`: auto-filed | explicit |
    empty-section | oversized | derived | none, plus `scope_files`, set
    on every arm) — pull composes the skip reason from scope and orders
    on it, backfill's actions read it: one classification channel,
    never a parallel vocabulary or a re-derived copy.

    TRUST (round 3, the security review's boundary): only an explicit
    `Files:` section makes a cluster trusted. Derived scope comes from
    the ticket's prose — and deferral tickets quote PR-authored text,
    so derived paths are attacker-influenceable until a human curates
    them; the curation step is backfill, whose --apply appends exactly
    the derived paths as an explicit section (auditable, one row per
    issue). Derived clusters ride the queue untrusted and the worker's
    trusted-key pick never reaches them."""
    if task.get("key"):
        task["scope"] = "auto-filed"
        task["scope_files"] = []
        return []
    files, scope = ticket_scope(task["description"])
    task["scope"] = scope
    task["scope_files"] = files
    if scope in ("empty-section", "none"):
        return []
    out = nightshift.cluster(_task_findings(task, files), FILES_PER_CLUSTER)
    if len(out) > TASK_CLUSTER_CAP:
        task["scope"] = "oversized"
        return []
    for c in out:
        # identity lands once, here, on the shaped clusters (the only
        # owner) — and it is INJECTIVE over this function's own output:
        # the file cap can split one crate into sibling clusters, so
        # the file set joins the key (content-derived; re-runs dedupe)
        c["key"] = hashlib.sha1(
            f"{task['identifier']}:{c['crate']}:{'+'.join(sorted(c['files']))}".encode()).hexdigest()[:10]
        c["task"] = task["identifier"]
        c["trusted"] = scope == "explicit"
        c["id"] = f"TASK-{task['identifier']}-{c['key'][:6]}"
    return out


def backfill(label: str, call, apply: bool) -> list[dict]:
    """LABS-684: make the section-less backlog explicit. For every open
    labelled issue with no `Files:` section but derivable path mentions,
    append the derived section (the same `_scan_paths` vocabulary, in
    the format parse_files reads). Issues that already carry a section, that
    are auto-filed clusters, or that mention no workspace path are
    reported and left alone — the report is the audit trail, one row per
    issue, action named."""
    res = []
    for t in pull(label, call):
        # the attached scope facts task_clusters already resolved — the
        # action names map onto the one classification (the ACTIONS
        # table, indexed directly so an unknown scope fails loudly),
        # never a re-derived copy of it. 'derived' is the one writing
        # arm; every other scope reports and leaves the ticket alone
        # (empty-section in particular is the human's explicit answer,
        # and appending a second section would loop on every --apply).
        if t["scope"] != "derived":
            res.append({"identifier": t["identifier"],
                        "action": ACTIONS[t["scope"]], "scope": t["scope"]})
            continue
        files = t["scope_files"]
        if not apply:
            res.append({"identifier": t["identifier"], "action": "would-append",
                        "files": files, "scope": t["scope"]})
            continue
        section = "\n\nFiles:\n" + "\n".join(f"- {p}" for p in files) + "\n"
        call(M_UPDATE, {"id": t["id"], "i": {"description": t["description"] + section}})
        res.append({"identifier": t["identifier"], "action": "appended", "files": files,
                        "scope": t["scope"]})
    return res


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    # per-command extras as a table (the COMMANDS idiom): the shared
    # flags stay shared, each command's own flags live beside its name,
    # and no per-command conditional rides inside the loop
    EXTRA = {
        "sync": [(("--team-key",), {"default": "LABS"}),
                 (("--queue",), {"required": True}),
                 (("--dry-run",), {"action": "store_true"})],
        "backfill": [(("--apply",), {"action": "store_true",
                                     "help": "write the derived Files: sections "
                                             "(default: dry-run report)"})],
    }
    for name in ("sync", "pull", "backfill"):
        p = sub.add_parser(name)
        p.add_argument("--label", default="review-tasks")
        p.add_argument("--out")
        for flags, kw in EXTRA.get(name, []):
            p.add_argument(*flags, **kw)
    a = ap.parse_args()

    if a.cmd == "sync" and a.dry_run:
        with open(a.queue) as fh:
            q = json.load(fh)
        res = [issue_input(c, "<team>", "<label>") for c in q["runnable"] + q.get("deferred_outside_window", [])]
    else:
        key = os.environ.get("LINEAR_API_KEY") or sys.exit("linear_queue: LINEAR_API_KEY not set")
        call = functools.partial(gql, key=key)
        if a.cmd == "pull":
            res = pull(a.label, call)
        elif a.cmd == "backfill":
            res = backfill(a.label, call, a.apply)
        else:
            with open(a.queue) as fh:
                q = json.load(fh)
            tid, lid = resolve(a.team_key, a.label, call)
            res = materialize(q, tid, lid, call)
    text = json.dumps(res, indent=1) + "\n"
    if a.out:
        with open(a.out, "w") as fh:
            fh.write(text)
    else:
        sys.stdout.write(text)


if __name__ == "__main__":
    main()
