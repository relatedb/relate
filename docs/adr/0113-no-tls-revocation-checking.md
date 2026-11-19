---
type: adr
area: cli
date: 2026-09-26
deciders: CypherLite maintainers
name: 0113-no-tls-revocation-checking
status: accepted
tags:
  - security
title: CLI HTTPS clients do not check certificate revocation
REFERENCES:
  - "[[adr/0080-cli-pull-command]]"
---

# 0113. CLI HTTPS clients do not check certificate revocation

## Context

Three CypherLite code paths make outbound HTTPS requests with `ureq` 3.4:

- `upgrade` fetches release metadata and the binary asset from the GitHub
  releases API (`surfaces/cypherlite-cli/src/upgrade_cmd.rs:353-354`),
  feature `upgrade`.
- telemetry POSTs usage envelopes to Mixpanel
  (`surfaces/cypherlite-cli/src/telemetry/handle.rs:245-255`), feature
  `telemetry`.
- `pull` talks to the Neo4j/Aura Query API v2 endpoint given by
 `NEO4J_URI` (`surfaces/cypherlite-load/src/http.rs:107`), per [ADR-0080](./0080-cli-pull-command.md).

All three use `ureq`'s default TLS configuration: rustls with bundled
`webpki-roots` as the trust anchors. That default correctly rejects expired,
self-signed, untrusted-CA, hostname-mismatch, and incomplete-chain
certificates. It performs **no CRL or OCSP revocation checking**: rustls does
not fetch revocation material at all.

Linear LABS-120 is tracked as OPLANE_REQ-00262977. Security review of the CLI
TLS posture flagged revocation as the sole failing primary check; every other
certificate-validation check already passes under the `ureq` defaults. This
ADR records the resolution of that finding.

The decision below was settled by the maintainer before this ADR was written;
this record states it rather than reopening it.

## Decision

**The CLI HTTPS clients keep `ureq` 3.4 with default rustls + bundled
`webpki-roots` verification and perform no CRL/OCSP revocation checking. This
is an accepted risk, not a gap to close.**

The accepted-risk justification:

1. **rustls performs no revocation fetching.** There is no first-class,
   upstream-supported revocation client for the rustls path `ureq` uses, so
   "enable revocation" is not a configuration toggle — it is new code and a
   new dependency.
2. **Browsers abandoned live OCSP.** Live OCSP checking soft-fails open in
   practice, and the industry moved to CRLite and short-lived certificates
   precisely because online revocation checks are unreliable and privacy
   hostile. Requiring what browsers no longer require would overstate the
   protection.
3. **Short-lived managed certificates shrink the exposure window.** The
   endpoints in question (GitHub releases API, Mixpanel, the configured Query
   API host) use short-lived managed certificates, so a revoked-but-unexpired
   certificate is a narrow window.
4. **The endpoints are pinned by configuration.** These are not arbitrary URLs
   pulled from user input: the GitHub releases API and Mixpanel hosts are
   fixed, and the Query API host comes from operator-supplied `NEO4J_URI`
   configuration.
5. **A network-position attacker can block revocation signals anyway.** An
   adversary able to serve a revoked certificate is already on the network
   path and can equally suppress CRL/OCSP fetches (or block them outright),
   so revocation checking does not remove that adversary's capability.

Only revocation is out of scope. Expired, self-signed, untrusted-CA,
hostname-mismatch, and incomplete-chain certificates remain rejected by the
`ureq` defaults, and no client, trust-store, or pinning change is made here.

## Alternatives considered

- **Adopt `rustls-platform-verifier`.** This swaps the bundled `webpki-roots`
  for the operating system trust stores. Rejected: that is a trust-anchor
  posture change (the set of roots that can vouch for these hosts becomes the
  local machine's, not the crate's pinned Mozilla set) rather than a
  revocation fix, and it adds a heavy dependency for a CLI where the current
  roots are already adequate.
- **Manage CRLs manually.** Download, cache, and apply CRL distribution points
  for every chain. Rejected: it is impractical for a CLI — it needs
  background fetching, freshness and expiry handling, cache storage, and
  failure policy, all to cover an exposure window that short-lived managed
  certificates already narrow, and it does not stop a network-position
  attacker from blocking the CRL fetch.

## Consequences

- The revocation finding in OPLANE_REQ-00262977 is resolved as an accepted
  risk; the remaining certificate checks stay enforced by `ureq` defaults and
  must not regress.
- No new dependency, trust-store switch, or pinning layer is added to
  `cypherlite-cli` or `cypherlite-load`; their `ureq` usage is unchanged.
- If a future requirement needs OS trust stores or revocation checking, it
  lands as a new ADR, not an edit to this one (accepted ADRs are immutable).
