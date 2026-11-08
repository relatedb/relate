# ADR-0544: Memory envelope improvements warn

Status: Proposed
Claim: d2b43b1fb1df1c9ae48c1696a34f045db6e63d3f

## Context

ADR-0526 and ADR-0539 fail the memory-controls and memory-shape jobs
when a lower-is-better metric falls below the frozen envelope. That
was meant to catch a broken measurement and to force a re-freeze
before a later regression could hide under a stale floor.

A real drop in memory use then blocks the pull request until a new
envelope lands. The comment already names the checks and asks for
that re-freeze. The failure does not add a signal the comment lacks.

## Decision

An improvement keeps its verdict. The compare document, the
pull-request comment, and the step summary still carry it. Inside
GitHub Actions each improvement check is a warning annotation. The
compare exits 0, so the job stays green.

A regression still fails. A run that is both a regression and an
improvement still fails. An incompatible environment class in CI
still fails. Behaviour checks have no improvement verdict, as
ADR-0526 set out.

## Alternatives considered

- Keep failing on an improvement, as ADR-0526 and ADR-0539 decided.
  Rejected: a real drop blocks unrelated work, and the warning
  already asks for the investigation and the re-freeze.
- Drop the improvement verdict and only gate regressions. Rejected:
  the envelope would go stale with no record of the drop.

## Consequences

- A pull request whose only misses are improvements stays green. The
  comment still warns, and a real drop still wants a new freeze.
- The nightly memory-controls and memory-shape runs use the same
  compare, so an improvement warns there too.
- The bottom of the envelope can sit above current use until someone
  re-freezes. The warning is the reminder. A regression above the
  top of the envelope still fails.
