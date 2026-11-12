# ADR-0571: The installer resolves its release via releases.json

Status: Proposed
Claim: 1bc98c705f6b583a2938f156599d882ca8524a75

## Context

`install.sh` (ADR-0068) resolves "the latest release" at runtime through
the GitHub API's latest-release endpoint, which returns the most recently
published full (non-draft, non-prerelease) release in the repository —
whichever release that is. The repository's release list is no longer only
product releases: the extension channel publishes each extension as a full
GitHub release here, tagged `relate-extensions-<name>-<X.Y.Z>` (ADR-0431),
and the releases.json publisher deliberately excludes those tags from the
core window (ADR-0250 §6). After an extension release is published, the
endpoint names the extension release and the installer dies with "has no
asset named relate-<os>-<arch>" until the next product release — the
one-liner install breaks on the repository's own release cadence.

`upgrade` left this endpoint behind in ADR-0250: release discovery reads
the static `releases.json` on the project file host, whose `latest` is the
newest stable core release by construction. The installer remained the
endpoint's last consumer and the only release consumer a release of
another kind can redirect.

The integrity posture does not change: `SHA256SUMS` is authenticated by
its SSHSIG signature against the pinned key (ADR-0115), the signed version
header must equal the resolved tag (ADR-0457), and `releases.json` only
selects what to download — it is never trusted for integrity.

## Decision

`install.sh` resolves the release tag from `releases.json` and asks the
GitHub API only for the asset ids of that exact tag.

1. **Resolution.** The metadata URL is
   `https://relatedb.ai/releases.json`, overridable through
   `RELATE_RELEASES_JSON_URL` with the same empty-means-default rule
   `upgrade` documents (ADR-0250 §1), and it is the only source consulted:
   no fallback. The request carries no `Authorization` header (the file is
   public and on a different origin, ADR-0250 §5). The tag is `latest.tag`
   and must be a `vX.Y.Z` release tag — the stable-tag invariant the
   schema fixes and the publisher selects by (ADR-0250 §2); anything else
   is a malformed document, exactly as the CLI's parser treats it.
2. **Asset ids.** One per-tag request (the releases/tags endpoint, never
   the latest-release endpoint), with the `GITHUB_TOKEN` bearer header
   while the repository is internal (ADR-0068 §6). The response's
   `tag_name` must equal the requested tag: the API must describe exactly
   the release the metadata named. The asset download route is unchanged.
3. **Failure behavior.** Every resolution failure dies before any
   download with a one-line message naming the failed source, mirroring
   ADR-0250 §4's aborts; the `GITHUB_TOKEN` hint moves to the per-tag
   fetch failure — the one request the token can help. The ADR-0457
   binding, the legacy window, and the verify-before-download ordering
   are unchanged; only the tag's provenance changes.
4. **Harness.** The offline harness pins the contract: the stub serves an
   extension release at the old endpoint in every mode, so any request of
   it fails the install loudly; a case serves the publisher's committed
   output fixture as the metadata document end to end, so a serialization
   the installer's parse cannot read fails the suite instead of the
   one-liner; and section-0 drift guards hold the URL literal and the
   env-var name to the CLI's `releases_json.rs` constants.

## Alternatives considered

- **Publish extension releases as GitHub prereleases** (the endpoint skips
  prereleases, so it would keep naming product releases) — rejected: it
  mislabels stable extension releases for every other consumer of the
  flag, and it makes the installer's correctness rest on a labeling
  discipline each future extension release must remember; one forgotten
  checkbox silently breaks the one-liner again. `releases.json` already
  encodes the core/extension split by construction (ADR-0250 §6).
- **Filter the API release list by tag shape** — rejected: it keeps the
  installer on the anonymous-rate-limited API ADR-0250 removed `upgrade`
  from, walks a paginated list ordered by creation rather than
  publication, and re-derives the publisher's core-window selection — a
  second copy of the filtering policy that can drift from the publisher's.
- **Skip the GitHub API entirely via `upgrade`'s probe-then-fallback
  routes (ADR-0358)** — rejected: while the repository is internal the
  direct route 404s regardless of token (ADR-0358's own finding), so the
  probe is a guaranteed wasted fetch, and the fallback arm still needs the
  per-tag API response. A future ADR can adopt the direct route once the
  repository is public.

## Consequences

- The installer's first request goes to the project file host instead of
  `api.github.com`. That host already serves the installer itself
  (ADR-0068), so resolution now fails with it exactly as `upgrade` already
  does — one hosting dependency shared by both consumers instead of two.
- `RELATE_RELEASES_JSON_URL` gains a second consumer at install time, and
  the drift guards hold the two consumers to one URL and one env-var name,
  so they cannot drift onto different metadata sources.
- Extension releases can no longer break the one-liner, whatever the
  release cadence: resolution follows the publisher's core-window
  selection, and the harness's trap arm fails loudly if the old endpoint
  ever returns.
- The installer gains no tool dependencies; the resolution failure
  messages are reworded to name their source.
- ADR-0068's runtime-resolution consequence gains its source; its hosting,
  URL, install directory, platform scope, and asset-request auth model are
  unchanged, recorded in a Later references note there. ADR-0457 carries
  the same note for the tag's provenance.
- Moving asset downloads off the GitHub API (ADR-0358's direct route once
  the repository is public) remains a future decision; this one only
  changes resolution.

## Later references

ADR-0585 takes the future decision reserved above: the installer's asset
downloads ride the direct route, and §2's per-tag asset-id request —
with the token hint §3 moved to it — is gone from the installer
entirely.
