# ADR-0598: The extension stream cap guards whole-import framing only

Status: Proposed
Claim: bf165f653148e191305618821168437c506ebd8e

## Context

The extension mount caps a child's whole MCP request stream at 128 MiB
(ADR-0430 §4, carried by ADR-0442 and ADR-0447). The git importer commits
per batch (ADR-0392): each writing call is its own transaction, and §3 of
that ADR pins the contract that re-running the same command over a
partially imported store completes the history.

At real-history scale those decisions collide. A history the importer's
own caps admit streams more wire bytes than the whole-run cap admits: a
pair row carries two full stored paths, and the wire costs roughly 1.7×
the stored bytes. The refusal is deterministic, so the identical re-run
fails at the identical byte — the completion contract does not hold
through the mount, and the partial left behind looks nearly complete
while holding none of the coupling data the import exists to produce.

The cap's stated purpose — bound what an untrusted child can make the
host hold — is served per call already: every line by the framing cap,
every call by the rows and statement caps. The whole-run counter guards
exactly one thing they cannot: the framing that retains work across
calls. A `whole-import` session holds every write in one transaction's
overlay until its single commit. A `per-batch` session retains nothing
between calls — its retained work is the store, bounded by the
live-graph limit the write engine enforces with a typed refusal.

## Decision

The total stream cap applies to `whole-import` sessions only. A
`per-batch` session has no whole-run stream cap: its bounds are the
per-call caps and the host store's live-graph limit.

Both sides scope by the same one fact — the commit policy the manifest
declares and the handshake publishes. The host counts request bytes only
under `whole-import` framing. The child kit counts its written bytes
under the same policy and against the stream cap the handshake
publishes, so a long per-batch import no longer refuses child-side at a
byte the host would admit, and a whole-import child still refuses before
the host kills it.

The 128 MiB number, the fatal classification, and the published
`maxStreamBytes` fact are unchanged. Both stream-cap refusals gain
ADR-0377 §4's actionable shape: the limit as a number, the framing it
guards, and the two ways out — window the import, or move the package to
`per-batch` framing. No mount flag changes the cap; the rejected
alternatives record why none exists.

## Alternatives considered

- **Intern paths on the wire (a per-import dictionary plus integer
  handles).** Roughly an order of magnitude off the pair stream, and the
  better end state for wire cost. Deferred, not rejected as a direction:
  it is a wire revision with its own session-state surface, a separately
  reviewable decision — and it changes cost, not the contract; under any
  fixed whole-run cap some admitted shape still crosses it.
- **Raise the number.** Rejected: it moves the failure to a larger repo,
  keeps the deterministic re-run breach, and every candidate number is
  arbitrary against inputs the importer's own caps already bound.
- **A `--max-stream-bytes` flag on `relate run`.** Rejected: ADR-0377's
  recorded stance — a safety rail every user must remember is not a
  tuning knob — plus the flag-table cost across help, spec and bundles;
  and a local opt-out leaves every other run with the broken contract.
- **Remove the cap for every policy.** Rejected: the whole-import
  session's one transaction retains every write until its single commit,
  and the whole-run cap is the only pre-commit bound that framing has.
- **A store-side whole-run budget for per-batch sessions.** Rejected:
  the live-graph limit already is that budget, enforced by the write
  engine at admission — a second budget would re-derive the one the
  store owns.

## Consequences

- A per-batch import completes histories whose wire bytes exceed 128
  MiB, and a re-run over its partial store completes the history:
  ADR-0392 §3's contract holds through the mount. Pinned host-side (a
  per-batch session whose cumulative request bytes cross a tight stream
  cap admits every call) and child-side (a per-batch child writes past a
  tight published cap; a whole-import child refuses with the new text).
- A `whole-import` session keeps the whole-run cap and gains the
  actionable refusal. Tests pin the exact wording on both sides; the
  extensions spec's caps section, fatal-refusal list and exit table, and
  the bundled extensions reference, move with them.
- A per-batch child's runaway bounds are the per-call caps plus the
  store's live-graph limit, refused typed per call — the same bounds the
  product MCP server's writers run under.
- ADR-0430 §4 and ADR-0434's importer-bounding consequence gain `Later
  references` notes naming this ADR.
- No new flag, tool, schema change, or dependency; the published cap set
  is byte-identical.
