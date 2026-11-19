---
type: adr
area: ecosystem
claim: d2ff6c2053376cc9a02d291b708b8f2a1ee62fa5
name: 0300-adr-number-mint-counter-branch
status: accepted
tags:
  - adr-process
title: ADR number mint via a counter branch
REFERENCES:
  - "[[adr/0242-agent-context-command-and-skill-drift-check]]"
  - "[[adr/0301-adr-claim-proof]]"
---

# ADR-0300: ADR number mint via a counter branch

## Context

AGENTS.md's decision-log rule — "Next number = highest existing prefix +
1. Do not guess; list the directory", and step 2 of "How to add an ADR"
(`ls docs/adr/*.md | sort | tail -1`) — makes every agent compute the next
ADR number from its own checkout. That was safe when ADRs were written
sequentially on `main`. With many agents working in parallel on separate
branches, two agents compute the same next number from the same base
checkout and collide at merge time: two PRs arrive with
`docs/adr/NNNN-<different-slug>.md`, and one must be renumbered after
review, after links and text already reference it. [ADR-0242](./0242-agent-context-command-and-skill-drift-check.md) carries such a
renumbering note. Linear issue LABS-318.

The fix must give each author the real, final number at authoring time,
must work from any checkout with only `git push` access, and must not let
a number be claimed twice — without requiring human coordination between
concurrent agents. One constraint comes from the transition itself:
numbers 0243 and above may already be taken on unmerged branches whose
authors picked them under the old list-the-directory rule (those numbers
do not exist on `main` yet), so the mint needs a floor above them.

## Decision

1. **A dedicated branch named `ADR-NUMBER-MINT-DO-NOT-DELETE`** holds a
   single file `NEXT` containing the next unclaimed four-digit ADR
   number. The branch is the single source of truth for numbering; it
   carries no other content.
2. **Minting is performed by `scripts/adr_mint.py`**: fetch the counter
   branch; compute `claimed = max(NEXT, highest NNNN prefix in docs/adr
   on main + 1, 0300)`; commit `NEXT=claimed+1` with a commit message
   recording the claim (e.g. `claim 0300`); push
   `<commit>:refs/heads/ADR-NUMBER-MINT-DO-NOT-DELETE`. Git ref updates
   are atomic compare-and-swap on the server: a non-fast-forward
   rejection means a lost race — refetch, recompute, retry (bounded at
   10 attempts).
3. **Floor rule**: a minted number is never below 0300 — the claimed
   number is `max(NEXT, main's max prefix + 1, 0300)`, and the counter
   initializes `NEXT = max(main's max prefix + 1, 0300)`. The 0243–0299
   range is headroom for in-flight branches that grabbed numbers under
   the old rule before the mint existed; a rebuilt counter also restarts
   at 0300, never lower.
4. **Self-heal rule**: the minted number is never below the max prefix
   already merged on `main`, even if `NEXT` lags behind because someone
   added an ADR without minting.
5. **Each pushed commit records the claimed number**, so the remote
   branch is an append-only ledger of claims; nobody else can claim the
   same or a smaller number.
6. **The mint claims and records the number only** — the caller names
   its own `docs/adr/NNNN-<slug>.md` file.
7. **A PR CI check** (`scripts/check_adr_numbers.py`, wired as an
   `adr-numbers` job in `.github/workflows/ci.yml`) flags conflicts from
   additions that bypassed the mint.

## Alternatives considered

- **Ref-per-number claims** (one branch per number, claimed via atomic
  GitHub ref creation) — rejected: it works, but spams one long-lived
  ref per ADR and needs garbage collection; the counter branch is a
  single ref.
- **Dedicated issue-counter repository** (GitHub issue numbers as the
  atomic sequence) — rejected: it couples ADR numbering to a second
  repository and burns its issue numbers; the counter branch stays
  in-repo.
- **GitHub Actions mint workflow** (a `workflow_dispatch` with a
  concurrency group serializing runs) — rejected: queue latency and
  polling for what is one atomic push; also unusable without GitHub
  Actions access, while every agent already has `git push`.
- **Assign-at-merge renumbering** (authors write `NNNN=XXXX`
  placeholders, a bot assigns numbers at merge time) — rejected: the
  number changes after review and after any link or text referencing
  it; authors want the real number at authoring time.

## Consequences

- The AGENTS.md decision-log bullet "Next number = highest existing
  prefix + 1" and "How to add an ADR" step 2 are superseded by "mint
  via `scripts/adr_mint.py`" (updated in the same change).
- `scripts/adr_mint.py` is the only sanctioned way to claim a number.
- The PR CI check (`scripts/check_adr_numbers.py`, `adr-numbers` job in
  `.github/workflows/ci.yml`) flags conflicts from additions that
  bypassed the mint.
- The branch `ADR-NUMBER-MINT-DO-NOT-DELETE` must never be deleted (the
  name says so).
- The `adr-numbers` job intentionally runs the PR's copy of the checker:
  it is a numbering-hygiene gate, not a security boundary — a PR that
  edits the checker only defeats its own ADR-number check.
- The `ADR-NUMBER-MINT-DO-NOT-DELETE` branch is protected by an active
  GitHub ruleset (deletion and force pushes blocked), so the claim
  ledger is append-only.
- Numbers stay four-digit and monotonic, and a deleted or lost counter
  branch can be rebuilt from `main`'s max prefix, restarting no lower
  than 0300.
- ADRs numbered under the old rule before the mint existed sit below
  the mint floor; this ADR is the first minted number (0300, claimed by
  `scripts/adr_mint.py` before this branch landed).

## Later references

- [ADR-0301](./0301-adr-claim-proof.md) adds proof-of-mint on top of this ADR's counter-branch
  ledger: every ADR added by a PR carries a `Claim:` header naming its
  claim commit, `scripts/adr_mint.py` prints that commit's SHA, and
  `scripts/check_adr_numbers.py` verifies the header against the ledger
  (missing header, off-ledger SHA, or a claim naming a different number
  all fail). This ADR's own bare `claim 0300` is grandfathered.
