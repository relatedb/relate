---
type: adr
area: infra
claim: 3e13599ec43762e743d8975d2efdb24bc7b798e0
name: 0354-nits-collapsed-appendix-not-threads
status: accepted
tags:
  - review
title: Nits never open inline threads; they ride a collapsed Low appendix
---

# ADR-0354: Nits never open inline threads; they ride a collapsed Low appendix

## Context

Every stellar finding opened an inline review thread, including nits
(rendered "Low"). A nit never blocks the verdict (FAIL_SEVERITIES is
blocker/medium) and is capped at emission, yet its inline thread demands
per-thread triage — reply, resolve, disposition — and gets re-reported in
every later round's "still open" list. PR #346 carried 28 such threads;
the triage cost exceeded the findings' value, and the recurrence loop
(LABS-311) needs the *data*, not the *threads*.

## Decision

The poster threads only findings that can fail the verdict: anchored
blocker/medium findings open inline threads. Anchored nits never do —
they print exactly once, in a collapsed `<details>` Low appendix at the
end of the round's summary, superseded automatically by the next
round's list. Finding *emission* is unchanged: nits still reach the
ledger and `recurrence.py`, so the prevention metrics keep them.

## Alternatives considered

- **Stop emitting nits at the passes** — rejected: the recurrence data
  (which patterns are becoming systemic) lives in the findings document;
  dropping emission silently loses it.
- **Keep threading nits, rely on reply-and-resolve triage for all** —
  rejected: works (demonstrated on PR #346) but taxes every round with
  per-thread ceremony for findings that cannot block; the appendix gets
  the same information across with zero triage debt.

## Consequences

- Inline threads mean "a human should look at this"; Lows mean "noted,
  collapsed above".
- The verdict math is unchanged (nits never counted).
- The triage flow (fixed in <sha> / deferred → Linear / refuted) applies
  to blocker/medium threads; a nit that matters promotes via its
  recurrence, not via a thread.
