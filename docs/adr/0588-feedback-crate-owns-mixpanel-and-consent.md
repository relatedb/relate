---
type: adr
area: surface
claim: 47fe8c879275aa42286f25d3d322ab7215b4d08e
name: 0588-feedback-crate-owns-mixpanel-and-consent
status: proposed
tags:
  - telemetry
title: One feedback crate owns the Mixpanel client and consent
REFERENCES:
  - "[[adr/0079-anonymous-usage-metrics]]"
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0263-telemetry-endpoint-https-only]]"
  - "[[adr/0545-telemetry-spool-delivery]]"
---

# ADR-0588: One feedback crate owns the Mixpanel client and consent

Revision (2026-11-16, still Proposed): the maintainers decided — feedback
reports into the **one** shared Labs Mixpanel project (the event name, not a
second project, separates the stream; `RELATE_FEEDBACK_TOKEN` remains the
migration path to a dedicated project); the grant ask defaults to **yes**
and resurfaces once per grant window on the install and upgrade surfaces;
procedure names in known namespaces are identifiers, not host names, for
the redaction policy.

## Context

The CLI already reports anonymous usage events to Mixpanel from
`surfaces/relate-cli/src/telemetry` (~2.4k lines): the event taxonomy, the
layered opt-out, a spool with a drain thread, and a hand-rolled `/track`
POST ([ADR-0079](./0079-anonymous-usage-metrics.md), [ADR-0263](./0263-telemetry-endpoint-https-only.md), [ADR-0545](./0545-telemetry-spool-delivery.md)). A second channel is joining it: feedback,
in two mode-gated forms — a person's prefilled public GitHub issue and an
agent's consented report (`enable`/`disable` grant the agent's) — with its
own report model, local validation and redaction, a consent record, local
receipts, and one POST per report.

Both send JSON to a Mixpanel `/track` endpoint under the same rules: an
endpoint allow-list, a short deadline, no retries, redirects never followed,
`ip=0`, and dedup by insert id. Two copies would drift apart and grow the
CLI binary crate instead of giving the feature a home. [ADR-0079](./0079-anonymous-usage-metrics.md) rejected a
separate telemetry crate only because the CLI was its only consumer; it is
here, and [ADR-0103](./0103-engine-surfaces-testing-layout.md)'s layout lets a `surfaces/*` crate depend on `relate/*`
while the CLI depends on it, never the reverse.

## Decision

1. **One crate.** `surfaces/relate-feedback` owns:
   - the Mixpanel client: the `/track` POST (batched bodies, `ip=0`, a short
     deadline, no retries, redirects never followed, `$insert_id` dedup),
     response classification, and the one endpoint-allow-list
     implementation — `https` for the hosts a caller's policy names, and
 `http` only for loopback ([ADR-0263](./0263-telemetry-endpoint-https-only.md)'s rule as the floor);
   - the consent and opt-out model: the layered on/off resolution
     (`DO_NOT_TRACK`, environment, config key), the one-time notice claim,
     the telemetry state file, and the feedback consent record (endpoint,
     authorized-until);
   - the feedback domain: report model and limits, the validation and
     redaction policy, receipts, and submit orchestration.

   The crate takes the config directory and an environment lookup as
   arguments; the CLI keeps the one config-directory rule.
2. **What telemetry keeps.** The telemetry module keeps the event taxonomy
   with its compile-time privacy contract, the envelope builder, the spool
   and drain-thread lifecycle, the verbs, and the e2e suite; the POST path,
   endpoint resolution, opt-out resolution and state read/write (the install
   id included, one file one owner) move. No event or property changes, and
   `e2e_telemetry` passes unchanged: the consolidation is plumbing. One
   opt-out read is tightened — a `telemetry` key that is present but not a
   boolean now reads as untrusted and fails closed, where the previous
   reader fell through to the enabled default.
3. **Two channels, mode-gated; two Mixpanel projects, one client.**
   People file public GitHub issues through the repository's prefilled
   new-issue form: the first line of the text (or, for a one-line text, an
   80-character prefix cut back to a whitespace boundary) is the title and
   the full text the body; nothing leaves the machine until the person
   submits the form there, and the CLI makes no request itself. Agents post
   to the ingest endpoint under a person-granted standing record: every send
   is validated, redacted and receipted, and the telemetry opt-out ladder
   suppresses it before any send. The surfaces are mode-gated — the person's
   command exists only while agent mode is off, the agent's only while it is
   on, and each absent surface is an unknown command naming the other. The
   grant keeps the lifecycle-verb grammar: `enable` and `disable` are
   actions, a bare command prints the standing state, and `enable` runs only
   at a person's terminal, naming the destination and the 30-day window
   as it records; the ask offers the same grant, default **yes** (the
   revision above), in two shapes — once per install on the first
   interactive feedback command, and once per 30-day window on the
   `skill install` and `relate upgrade` surfaces, so a lapsed or declined
   grant is asked about again near its end; a telemetry ladder that
   resolves off suppresses every ask. The honest scope is cooperative-agent governance: the grant steers an
   agent that respects it, and a process attached to a pty looks like a
   person — the ask and the terminal rule are courtesies, not enforcement.

   Telemetry keeps its `RELATE-CLI_`
   event prefix, with the existing `RELATE_MIXPANEL_TOKEN`/
   `RELATE_MIXPANEL_ENDPOINT` overrides. Feedback reports
   `RELATE-CLI_AGENT_FEEDBACK` into the **one** shared Labs project (the
   revision above): the event name separates free-text reports from
   anonymous usage counts in queries and dashboards, and the token's one
   home is this crate — telemetry re-exports it — with
   `RELATE_FEEDBACK_TOKEN` and `RELATE_FEEDBACK_ENDPOINT` overrides as the
   migration path to a dedicated project later.
   Ingestion tokens are client-side and public in every shipped binary by
 design ([ADR-0079](./0079-anonymous-usage-metrics.md) §7); nothing injects a token at build time, and an
   explicitly empty `RELATE_FEEDBACK_TOKEN` override means no default
   destination, so only a loopback endpoint can send. The endpoint is
   resolved when the person authorizes
   (`relate feedback enable`); the token itself is read at send time. Each
   surface passes its endpoint policy as data: telemetry accepts any `https`
   host, feedback only the Mixpanel hosts. A later environment change cannot
   divert a recorded destination: submit sends to the recorded endpoint.
4. **No serve path: `relate agent-feedback` is the agent's send.** An agent
   reports through `relate agent-feedback`, taught by the CLI skill bundle's
   feedback reference: draft the friction, ask the person, send only after
   their approval. The person's `relate feedback` files a GitHub issue and
   sends nothing. There is no hidden `serve` command, no JSON-RPC loop, and
   no `feedback_submit` tool. The ported implementation registered an MCP
   server with the agent harness, and the tool earned its keep only with
   that registration machinery; the port drops the registration, so a server
   no host launches would be a second, undiscoverable send path over the
   same pipeline. The skill bundle is the discovery surface.
5. **Bundles.** Both commands and the crate ship in every bundle: a
   first-party command group of the product binary, mode-gated and never
   feature-gated. An agent harness cannot rely on a report channel the
   smallest build lacks, and the verbs carry no behaviour an operator would
   opt into — consent is the person's, refused at the verb. So there is no
   `feedback` Cargo feature, and `relate-feedback` (with the one clock the
   verbs stamp) is an ordinary dependency of the CLI crate; feedback state
   stays under `feedback.*` keys in the CLI's config file.

## Alternatives considered

- **Keep the client in the CLI crate and add a second one for feedback** —
  rejected: two endpoint policies, deadlines, redirect rules and dedup
  paths to keep in step, and the CLI crate grows instead of the feature
  gaining a home.
- **A `relate-feedback-client` crate under `relate/`** — rejected: it is not
  engine code and no engine crate needs it; the layout gives `surfaces/` to
  CLI adapters, and R3 lets the CLI depend on a surface.
- **An off-the-shelf Mixpanel SDK** — rejected as [ADR-0079](./0079-anonymous-usage-metrics.md) rejected one:
  `/track` is a single JSON POST over the workspace's existing HTTP client.
- **A dedicated MCP tool as the agent's send** — rejected: it is reachable
  only through the agent-harness registration the port does not carry, so
  it is a second send path no host launches; the skill bundle already
  teaches the CLI verb over the same pipeline.
- **One feedback command for both modes** — rejected: the person's form
  makes no request and the agent's send posts free text, so one command
  forces either a consent prompt into a person's one-liner or an unguarded
  send path into a script's reach.
- **Fold feedback into the telemetry module instead of a crate** — rejected:
  consent semantics differ (a person authorizing free-text reports versus
  anonymous counts), and the module is already at the size the crate split
  exists to prevent.
- **Build-time token injection, as the ported implementation's linker flags
  do** — rejected: nothing in the Rust release path injects a token, so a
  release build would depend on the build host.
- **One endpoint policy for both surfaces** — rejected: tightening
 telemetry breaks its accepted override rule ([ADR-0263](./0263-telemetry-endpoint-https-only.md)), and loosening
  feedback would let an agent-adjacent path redirect free-text reports to
  any `https` host.

## Consequences

- New workspace member `surfaces/relate-feedback`; `cargo xtask
  check-boundaries` covers the new edge, and the CLI's dependency on it is
  one-way.
- `e2e_telemetry` passes unchanged against the moved client; telemetry's
  documentation, environment variables, and state layout stay. The change is
  request-level visible: its POST now carries `ip=0` and follows no redirect.
- The CLI gains documented `feedback` and `agent-feedback` commands and two
  environment variables; the spec, a runnable example, and the skill
  bundle's reference land in the same change.
- No `feedback` feature exists to switch the verbs off: they compile in
  every bundle, and the report's code vocabulary comes from the exit-code
  table rather than an `agent-context`-only module.
- One Mixpanel project serves both streams (the revision above): the
  existing shared Labs project, with a schema pinning each event's
  properties — a dedicated feedback project needs only the token override
  and a re-grant, never a code change.
- Reports are free text: only the validated, redacted payload leaves the
  machine, and a locally rejected report records no payload in its receipt.
