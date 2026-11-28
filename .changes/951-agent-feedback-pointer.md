### Added

- The agent-feedback pointer is now hard to miss (ADR-0588 revision): an
  agent-mode run's first failing diagnostic carries a hint prefilled with
  the failing command and its error code, worded by the grant — a live grant
  gets the report-first, approval-gated instruction with a ready
  `relate agent-feedback` command; no grant points at `relate feedback
  enable`; an opted-out telemetry ladder shows nothing. `relate
  agent-context` prints a `Feedback:` block, agent-mode `relate --help`
  ends with one pointer line, and the skill bundle leads with an imperative
  "report friction before moving on" section (draft the friction, show the
  person, send only after their approval).
