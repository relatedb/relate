# ADR-0585: The installer downloads release assets over the direct route

Status: Proposed
Claim: 149adbb71aa163c5711d79edb6162140bf2671a7

## Context

`install.sh` (ADR-0068) resolves its release tag from `releases.json`
(ADR-0571) but then asked the GitHub API for that release's asset ids
and downloaded every asset through the API — the mechanism ADR-0068 §6
fixed while the repository was internal, where the browser download
route answers 404 regardless of token (ADR-0358's measurement). The
repository is public now, so those requests were anonymous, and
anonymous GitHub API requests are limited to 60/hour per IP address.
Users behind shared egress addresses — VPNs, corporate NAT, Cloudflare
WARP — share that quota with everyone else on it and routinely see it
exhausted: reported against the v1.0.0 release, the one-liner died
resolving the release and advised `GITHUB_TOKEN` for a private
repository, a cause that no longer exists.

`releases.json` already names every asset the installer needs; on the
public repository the direct download URL is determined by the tag and
the asset name alone. ADR-0571 reserved this move — "a future ADR can
adopt the direct route once the repository is public" — and `upgrade`
made it in ADR-0358, whose own record names the shared-IP anonymous
quota as the failure the direct route removes.

## Decision

`install.sh` consults no GitHub API endpoint. Every asset — the
`SHA256SUMS` manifest, its `SHA256SUMS.sig` signature, and the platform
binary — is downloaded from
`https://github.com/<repo>/releases/download/<tag>/<asset>`, the direct
browser route `upgrade` probes (ADR-0358 §1), unauthenticated.

1. **One transport.** `releases.json` remains the only metadata source
   (ADR-0571); its tag and the asset names determine every URL. Any
   non-2xx answer dies with the request and the HTTP status — the same
   pinned status text the CLI's release channel carries. The transport
   is TLS-only: the seam's base is gated to `https://` before the first
   request of any kind — deliberately narrower than the CLI seam's
   `approved_transport` loopback plain-http carve-out, because the
   installer is the end-user pipe-to-bash surface and its own test rig
   drives the seam without real HTTP — and every download request is
   protocol-pinned across redirects, so no hop can downgrade to
   plaintext.
2. **No token.** `GITHUB_TOKEN` is not read: the direct route cannot be
   helped by a token at either repository visibility, and curl would
   forward a custom Authorization header across the route's cross-host
   redirect to the asset CDN.
3. **One seam.** The base is overridable through
   `RELATE_RELEASE_DOWNLOAD_URL` — the same seam `upgrade` documents —
   with empty meaning the canonical default and no fallback between the
   two; a harness drift guard holds it to the CLI's constant.
4. **The integrity chain is unchanged.** `releases.json` selects but is
   never trusted; SSHSIG verification (ADR-0115), the signed
   version-header binding (ADR-0457) and the checksum gate run before
   the binary is trusted, whichever route delivered the bytes.

## Alternatives considered

- **ADR-0358's probe-then-fallback (keep the authenticated API arm)** —
  rejected: the repository is public and the one-liner's audience holds
  no token; the arm would keep the API machinery, its JSON parse surface
  and its token plumbing alive for a case that no longer exists.
  `upgrade` keeps its own fallback; the installer does not need one.
- **Keep the API route, reword the failure** — rejected: the failure is
  structural — every tokenless install spent about four anonymous API
  requests against a shared 60/hour quota.
- **Carry asset ids in `releases.json`** — rejected (ADR-0358's own
  record): a publisher change plus a backfill, to save requests the
  direct route already eliminates.
- **Host the assets on the project file host** — rejected (ADR-0358's
  own record): GitHub release assets are the release contract; the file
  host serves metadata, not artifacts.

## Consequences

- A tokenless install makes zero GitHub API requests; shared-egress
  rate limits can no longer break the one-liner, and the private-repo
  advice leaves the installer.
- The installer's `RELATE_RELEASE_DOWNLOAD_URL` seam is https-only — a
  tested narrowing of the CLI seam's transport domain, not a second
  copy of its predicate.
- Installing against a private repository is no longer supported by the
  installer: the repository is public, the manual releases-page download
  and `upgrade`'s fallback cover the exotic cases, and a future
  visibility change needs a new decision.
- The installer loses its GitHub JSON parse surface (asset ids,
  `tag_name`) — the direct URLs need neither.
- ADR-0068 §6's authentication model is superseded for the installer,
  ADR-0571's reserved future decision is taken, and ADR-0358's "always
  the API asset route" characterization of `install.sh` becomes history
  — Later references notes on all three.
- The offline harness pins the no-API contract: its stub fails any
  `api.github.com` request loudly, and a drift guard rejects any API
  spelling in the script.
- The website serves the installer, so users see the fix when its copy
  of the script is redeployed.

## Later references

ADR-0586 extends the `https://` gate to the metadata seam
(`RELATE_RELEASES_JSON_URL`) in both installers, and pins `install.sh`'s
metadata fetch to https across redirects like its asset downloads.
