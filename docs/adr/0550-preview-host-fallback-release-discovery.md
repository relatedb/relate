# ADR-0550: Temporary preview-host fallback for release discovery

Status: Proposed
Claim: f74871b5d6ec755e0b768164aa1c698d4e5bf226

## Context

`relate upgrade` (and `upgrade --check`) discovers the latest release
through one pinned URL, `https://relatedb.ai/releases.json` (ADR-0250 §1).
Until the website launch the canonical host serves a lander page, not the
document, so the command cannot be exercised end to end before launch day.
The relate-web preview host already serves a valid document.

The preview host is temporary: it goes away at launch (days after this
lands), and this fallback goes with it.

## Decision

Release discovery reads the canonical URL first; when that read does not
yield a valid `releases.json` document — a transport failure, a read
failure, or a malformed document such as the lander page — it reads the
preview host's `releases.json` once, and only then fails. The failure
message keeps the ADR-0250 §4 shape (one line, the reinstall one-liner)
and names both URLs with each one's failure detail.

Both URLs have environment overrides (the existing
`RELATE_RELEASES_JSON_URL` seam and a new
`RELATE_RELEASES_JSON_FALLBACK_URL`), so tests point both at fixtures. The
fallback is never consulted when the primary yields a valid document.

The fallback applies to discovery only. Asset downloads still go through
the ADR-0358 routes, and the `SHA256SUMS` signature check (ADR-0115) is
unchanged: a hostile or broken preview host can influence which release is
offered, never which bytes run.

## Alternatives considered

- **Point the canonical constant at the preview host until launch.**
  Rejected: the pinned URL would flip twice in days, and a build cut in
  between would pin the throwaway host permanently.
- **Document the `RELATE_RELEASES_JSON_URL` seam for pre-launch testing.**
  Rejected: every tester must know and set it; the point of the fallback is
  that the stock command works during the preview window.
- **Keep the fallback after launch as a mirror.** Rejected: a second
  discovery origin is a standing trust and consistency surface; the preview
  host's removal date is the fallback's removal date.

## Consequences

- `upgrade` and `upgrade --check` work before launch without any setup.
- A removal change (code + a Later references note here) lands with the
  preview host's shutdown; the fallback constants, the second seam, and
  the both-hosts error shape all go away together.
- `releases.json` stays unsigned discovery metadata; the signed-manifest
  boundary (ADR-0115) is the integrity boundary, unchanged.
