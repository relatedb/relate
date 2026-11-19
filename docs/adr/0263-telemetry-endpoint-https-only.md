---
type: adr
area: cli
date: 2026-09-26
deciders: CypherLite maintainers
name: 0263-telemetry-endpoint-https-only
status: accepted
tags:
  - security
  - telemetry
title: Restrict the telemetry endpoint override to HTTPS
REFERENCES:
  - "[[adr/0079-anonymous-usage-metrics]]"
---

# 0263. Restrict the telemetry endpoint override to HTTPS

- **Renumbered**: from 0112 (shared-number cleanup)

## Context

[ADR-0079](./0079-anonymous-usage-metrics.md) §7 introduced `CYPHERLITE_MIXPANEL_ENDPOINT` as a dev/test override
that is used verbatim as the full POST URL "as-is", so development and CI can
point the CLI at a mock sink. The implementation keeps that promise literally:
`surfaces/cypherlite-cli/src/telemetry/handle.rs:80-81` assigns the environment
value straight into the endpoint and hands it to the background worker, with no
scheme or host validation. Any scheme is accepted, including a cleartext
`http://` URL to a non-loopback host.

Because the override is unvalidated, an operator who sets a non-loopback
`http://` endpoint causes the anonymous metrics — including the install ID and
the base dimensions — to be sent in the clear over the network. The default
sink is `https://api.mixpanel.com/track`, so the cleartext case only ever arises
from an override, and a mock sink for local development needs nothing more than
loopback HTTP.

Linear **LABS-120**, tracked as OPLANE_REQ-00262977, asks for the override to be
constrained so that an insecure endpoint cannot silently leak events. The
precedent for refusing cleartext is `warn_cleartext_basic` in
`surfaces/cypherlite-load/src/http.rs:74-92`, which warns when a URL is
`http://` and the host is not loopback.

The decision follows the repository rule that an accepted ADR is immutable:
[ADR-0079](./0079-anonymous-usage-metrics.md) §7's "as-is" behavior is changed by this new ADR, and [ADR-0079](./0079-anonymous-usage-metrics.md) carries
a **Later references** note pointing here rather than being edited.

## Decision

`CYPHERLITE_MIXPANEL_ENDPOINT` must be `https://` for any host, or `http://`
only when the host is loopback. The scheme and host are compared
case-insensitively, and IPv6 hosts are compared with the surrounding brackets
trimmed (so `http://[::1]/…` is accepted). The accepted loopback hosts are
exactly `localhost`, `127.0.0.1`, and `::1`.

A rejected override **disables telemetry for the run**: no worker thread is
spawned and no request is made. It prints exactly one stderr warning naming both
the variable and the rejected value:

```
warning: ignoring CYPHERLITE_MIXPANEL_ENDPOINT=<url>: http:// is allowed only for loopback hosts; telemetry disabled for this run
```

`<url>` is the rejected value, reproduced verbatim. The warning is the only
output; the CLI otherwise behaves as if telemetry were opted out for that run
(exit codes and stdout are unaffected, per [ADR-0079](./0079-anonymous-usage-metrics.md) §5).

This narrows only the override path. `CYPHERLITE_MIXPANEL_TOKEN` is unchanged,
and the default endpoint remains `https://api.mixpanel.com/track`.

## Alternatives considered

- **Silent fallback to the default endpoint.** Rejected: when an operator sets
  the override they intend a different sink; falling back to the compiled-in
  default would silently send events to the real Mixpanel project instead of
  the sink they asked for.
- **Silent disable (no warning).** Rejected: a developer pointing at a mock sink
  over a non-loopback `http://` URL would get no signal that their override was
  rejected, leaving them to debug why no telemetry arrived with no clue that the
  URL never took effect.
- **Accept any scheme and warn only on non-loopback `http://` while still
  sending.** Rejected: the point of the requirement is to prevent cleartext
  events, not merely to annotate them; a warning accompanied by an actual send
  would still leak.
- **Reject non-`https://` regardless of host.** Rejected: local mock sinks over
 `http://` loopback are exactly the dev/test workflow [ADR-0079](./0079-anonymous-usage-metrics.md) §7 exists to
  serve; loopback HTTP never crosses the network.

## Consequences

- An insecure override can no longer leak metrics: a non-loopback `http://`
  endpoint is refused and telemetry is disabled for that run.
- Every rejected override emits one stderr line, so a broken mock-sink URL is
  visible rather than silent.
- The validation logic (scheme/host parsing, case-insensitive comparison,
  IPv6-bracket trimming) and the exact warning string are a contract that the
  implementation and its tests must match; the warning text is pinned by this
  ADR.
- [ADR-0079](./0079-anonymous-usage-metrics.md) §7's "used as the full POST URL as-is" behavior no longer holds for
 non-loopback `http://` values; [ADR-0079](./0079-anonymous-usage-metrics.md) gains a **Later references** note
  pointing to this ADR.
- `docs/api/cli/spec.md` must describe the accepted/rejected endpoint forms if
  it documents the override's semantics.
