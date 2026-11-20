---
type: adr
area: cli
claim: 8f5bd338eef6e03ea624fd4d8f05d10ff335f71b
date: 2026-11-20
deciders: the maintainers
name: 0633-pull-bolt-uri-query-api-candidates
status: proposed
tags:
  - commands
  - http
  - load
title: "Bolt `NEO4J_URI`s resolve through ordered Query API origin candidates"
REFERENCES:
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0122-pull-auth-hardening]]"
  - "[[adr/0401-push-into-neo4j-aura]]"
---

# 0633: Bolt `NEO4J_URI`s resolve through ordered Query API origin candidates

## Context

[ADR-0080](./0080-cli-pull-command.md) fixed `pull`'s transport to the
HTTP Query API v2 and recorded one reinterpretation for Bolt URIs:
`neo4j://host:7687` maps to `http://host:7474`, and an Aura host
(`<id>.databases.neo4j.io`) serves the Query API on the bare
`https://<host>` with the bolt port stripped. It recorded nothing for a
non-Aura TLS Bolt URI, and the implementation filled the gap badly:
`neo4j+s://host:7687` built `https://host` — port 443, where a
self-hosted server has no Query API — and an explicit non-default bolt
port was passed through verbatim as the Query API port.

Where the Query API actually listens varies: a self-hosted server serves
it on its own port (https `:7473`, http `:7474`), while a fronting proxy
may serve it on the generic web port (`:443`, `:80`). A single static
mapping cannot serve both without forcing the user to restate the URI as
an explicit `http(s)` origin — the exact friction the mapping exists to
remove.

## Decision

1. The canonical mapping — what `query_api_url` returns and what the pull
   identity normalizes to — stays one origin per URI class: `http(s)`
   URIs as-is; an Aura host the bare `https://<host>`; a plaintext Bolt
   URI `http://host:7474`; a TLS Bolt URI (`neo4j+s`, `neo4j+ssc`,
   `bolt+s`, `bolt+ssc`, the 7687 port optional) `https://host:7473`.
2. The request layer resolves through a new ordered candidate list:
   `http(s)` and Aura URIs carry one candidate; a plaintext Bolt URI
   carries `http://host:7474` then `http://host` (port 80); a TLS Bolt
   URI carries `https://host:7473` then `https://host` (port 443). The
   server's own port comes first: any HTTP answer wins the resolution,
   and the generic web port is the one likely to host an unrelated
   service, so it must not preempt the server port when both answer.
3. The shared Query API POST prelude walks the candidates: the
   cleartext-Basic policy of [ADR-0122](./0122-pull-auth-hardening.md)
   runs once, on the canonical origin (every candidate of a URI shares
   one scheme and host, so the verdict is the same for each); then one
   POST per candidate, where **any** HTTP response — 2xx or a `401` —
   selects that origin. Only a failure that **proves the request never
   left the client** — a refused or unreachable connection, an
   unresolved host, a connect (TLS handshake included) or resolver
   timeout — advances to the next candidate. Any other failure (a
   global or body timeout, a reset or aborted connection, a TLS or
   protocol error) may mean the server applied the request, and a
   repeat POST could duplicate a non-idempotent write (the push path,
   [ADR-0401](./0401-push-into-neo4j-aura.md)) — so it fails the
   request instead of advancing. Every candidate failing undelivered is
   one error naming each origin tried with its failure.
4. A pull's identity source keeps the canonical origin (the first
   candidate): a `neo4j+s://host` pull and a `https://host:7473` pull are
   the same source. The fallback origin is a reachability detail, not a
   separate source.
5. ADR-0080's refusal for an explicit non-default bolt port stands (no
   candidate list); a port that is present but not a number refuses with
   fixed text, as does an invalid database name at the URL boundary.
   Candidates only ever vary the port on the host the URI itself names —
   no other host is contacted.

## Alternatives considered

- Keep one static mapping per class (server ports only): rejected — a
  proxied `:443`/`:80` origin fails while the server is plainly running,
  and the user must restate the URI as an explicit `http(s)` origin.
- Resolve once per run with a TCP reachability probe, then pin the
  winner: rejected — a listening socket is not proof of a Query API, the
  probe duplicates the failure signal the request itself already
  produces, and pinning threads resolved state through every request
  signature. The per-request walk costs one refused connect per request
  only when the canonical port is dead, and a refused connect transmits
  nothing.
- Try the generic web port first (`:443`/`:80`): rejected — behind a
  proxy both ports answer, and the generic port is the likelier wrong
  service; the server's own port must win the tie.

## Consequences

- A Bolt URI of either family pulls (and pushes — [ADR-0401](./0401-push-into-neo4j-aura.md)
  shares the resolution) against the server port or the proxied generic
  port with no user action; the hosted demo's
  `neo4j+s://demo.neo4jlabs.com:7687` reaches `https :7473`.
- When the canonical port is dead, each request pays one refused connect
  to it before the fallback answers; Basic credentials are only ever
  sent to the host the URI names (a refused connect transmits nothing).
  A failure that does not prove the request was undelivered — a
  timeout, a reset connection, a TLS or protocol error — is never
  retried on another origin: the pull (or push) fails with that error,
  and the explicit `http(s)` spelling remains the way to pin an origin.
- A non-Query-API HTTP service on the canonical port still wins the
  walk and surfaces its own protocol errors — loud, and the explicit
  `http(s)` spelling remains the way to pin an origin.
- The CLI spec, the pull example, and the CLI skill bundle's pull
  reference record the candidate order.
