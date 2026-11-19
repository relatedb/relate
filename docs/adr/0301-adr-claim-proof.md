---
type: adr
area: ecosystem
claim: d5d2272c53bced231f5ea777fc65321c46fcddae
name: 0301-adr-claim-proof
status: accepted
tags:
  - adr-process
title: ADR claim proof via the mint ledger
REFERENCES:
  - "[[adr/0300-adr-number-mint-counter-branch]]"
---

# ADR-0301: ADR claim proof via the mint ledger

## Context

[ADR-0300](./0300-adr-number-mint-counter-branch.md) moves ADR numbering to an atomic mint on the append-only
`ADR-NUMBER-MINT-DO-NOT-DELETE` ledger and adds a PR check
(`scripts/check_adr_numbers.py`, the `adr-numbers` job in
`.github/workflows/ci.yml`) that flags an added `docs/adr/NNNN-*.md`
whose number already exists on the base ref. That closes the collision
it was built for, but the check leaves two holes.

First, the verdict is correct only at run time. It compares the added
number against the base ref as it exists when the job runs, and a merge
does not re-run the checks of the other open PRs: two PRs that each
looked clean against `main` can still land together and collide. A stale
green is indistinguishable from a real pass, and closing the hole with a
merge queue or a required "up to date with main" branch would
re-validate every PR on every merge — heavy machinery for a numbering
rule.

Second, a bare `claim NNNN` ledger commit proves a number was minted,
but not by whom or for which file. Nothing ties a PR's
`docs/adr/NNNN-*.md` to the claim for that number: the check can only
compare against `main`'s state, never against the ledger. The
authoring-time guarantee [ADR-0300](./0300-adr-number-mint-counter-branch.md) promises — the author holds the real,
final number — is not verifiable after the fact.

The fix chosen in conversation is proof-of-mint: each ADR names the
ledger commit that claimed its number, and the check verifies that name
against the ledger.

## Decision

1. **Every PR that adds an ADR carries a `Claim: <40-hex>` header line**
   in the ADR file, after the `Status:` line and before `## Context`.
   The SHA is the claim commit for that number on
   `ADR-NUMBER-MINT-DO-NOT-DELETE`.
2. **`scripts/adr_mint.py` prints the claim SHA** as the second stdout
   line; the claimed number stays the first line, so a caller that reads
   only the first line is unaffected.
3. **`scripts/check_adr_numbers.py` fails an added ADR** whose header is
   missing, whose SHA is not an ancestor of the counter-branch tip, or
   whose claim commit's subject is not `claim <same number>`.
4. **The `adr-numbers` CI job explicitly fetches the counter branch**
   before running the checker, because the default checkout does not
   carry `ADR-NUMBER-MINT-DO-NOT-DELETE`.
5. **Validity is time-independent.** The proof references the
   append-only ledger, not `main`'s moving state: once a claim commit is
   on the ledger, "this number was minted" is a fact that cannot go
   stale when other PRs merge, so a green cannot be invalidated by a
   later merge and no merge queue is needed for ADR numbers. Exclusivity
   lives at authoring time in the mint's compare-and-swap push
 ([ADR-0300](./0300-adr-number-mint-counter-branch.md)), so a second PR can never mint the same number.
6. **This ADR is the first proof-carrying one**: its own `Claim:` header
   proves itself, and the checker verifies it like any other.

## Alternatives considered

- **Bind the claim message to the branch** (`claim NNNN for <branch>`) —
  rejected: it puts per-PR ceremony into the ledger's grammar, needs
  rename and grandfather handling, and the checker would still parse a
  free-form message; the header SHA is unambiguous.
- **Ledger-aware taken set without a merge queue** — rejected: reading
  the ledger's claimed numbers closes the base-ref blind spot, but the
  check is still a snapshot, leaving a check→merge race window; making
  it airtight needs a merge queue, which the proof avoids.
- **One ref per number pointing at the PR head commit** — rejected: a
  rebase moves the PR head and orphans the ref, so the proof would need
  re-pointing; it also adds one long-lived ref per ADR and ref hygiene
  overhead that the counter branch's single ref avoids.
- **Merge queue / require-up-to-date on `main`** — rejected: avoided by
  design; re-running every check on every merge is heavy machinery for
  a numbering rule, and the proof makes merge-time freshness
  unnecessary for ADR numbers.

## Consequences

- [ADR-0300](./0300-adr-number-mint-counter-branch.md) carries its real claim SHA (added when this requirement
  landed; it was minted before the mint printed the SHA, and its claim
  commit is on the ledger). ADRs older than the proof requirement are
  never re-checked: the checker only examines the ADRs a change adds.
- This ADR is the first proof-carrying one; its `Claim:` header names
  the `claim 0301` commit.
- Renumbering an ADR requires a fresh mint and a header update — a
  proof binds one file to one number.
- Fork PRs cannot mint: the mint pushes to the ledger, which a fork
  lacks access to; a maintainer mints on the contributor's behalf and
  the header names that claim.
- Direct pushes to `main` bypass the check entirely (no branch
  protection there); the proof is enforced on PRs, not by the
  repository.
- The `adr-numbers` job stays a numbering-hygiene gate, not a security
 boundary: it runs the PR's own copy of the checker ([ADR-0300](./0300-adr-number-mint-counter-branch.md)), and a
  PR that edits the checker only defeats its own check.
