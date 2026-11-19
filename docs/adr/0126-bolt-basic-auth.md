---
type: adr
area: surface
date: 2026-09-28
deciders: CypherLite maintainers (agreed in conversation)
name: 0126-bolt-basic-auth
status: accepted
tags:
  - auth
  - bolt
title: Bolt basic auth with a generated token default
REFERENCES:
  - "[[adr/0109-bolt-tls]]"
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0139-bolt-failed-hello-delay]]"
---

# 0126. Bolt basic auth with a generated token default

## Context

The Bolt server accepted every HELLO without looking at the auth token
no auth, no TLS, no connection governance; TLS landed in [ADR-0109](./0109-bolt-tls.md),
connection caps in [ADR-0110](./0110-bolt-connection-query-governance.md)). A server anyone reaches with `-p` or a
non-loopback bind takes credentials-free writes — and with silent acceptance,
a client that *thinks* it authenticated is the worst of both worlds. The
existing container UX (`make docker-run`) also puts the server on
`0.0.0.0:7687` by default, so "never exposed" is not a safe assumption.

## Decision

`cypherlite-bolt` gains basic authentication, resolved in order:

1. `--auth user:password` (flag wins)
2. `CYPHERLITE_AUTH=user:password` (env — the Docker `-e` path)
3. neither: a **generated token** (user `cypherlite`, 128-bit random hex),
   printed at startup (the Jupyter/Elasticsearch enrollment pattern) and
   persisted next to a persistent database — `<dir>.auth-token`,
   `<bundle>.cl.auth-token` (mode 0600, atomic temp+rename) — so a
   volume-backed server keeps the same credentials across restarts;
   `:memory:` generates fresh every start

`none` (flag or env) disables authentication explicitly — the pre-ADR
behavior where HELLO credentials are ignored.

Validation runs at HELLO (Bolt 4.4 carries the auth token in the HELLO map):
`scheme == basic` and an exact principal+credentials match, else
`Neo.ClientError.Security.Unauthorized` and the connection closes.

## Alternatives considered

- **Keep ignoring credentials** — rejected: silent "any credentials work"
  reads as secured to anyone with Neo4j muscle memory, and the security
  review of the container image had to caveat "publish ports only on trusted
  networks".
- **Hash/argon2 storage for the preset** — deferred: the preset is a
  deployment credential, not a user database; rotation means redeploy.
  Multi-user authz is out of scope (LABS-79's later slices).
- **Bolt 5.1+ LOGON negotiation** — the server negotiates 4.4, where the
  auth token rides in HELLO; LOGON support is a protocol-version question,
  not an auth question.

## Consequences

- The default gains a generated token: clients must connect with
  `auth=("cypherlite", <token>)` unless the operator passes `--auth none`.
  This is a deliberate default change — zero-config local runs opt back into
  no-auth with one flag.
- Docker needs nothing new: the env passes through the container, and the
  startup banner shows the token (logs) or its persisted path.
- `--help` text for `--auth` is the source of truth for the flag contract.

## Later references

- [ADR-0139](./0139-bolt-failed-hello-delay.md) adds a fixed 1000 ms delay before the
  `Neo.ClientError.Security.Unauthorized` response on a failed HELLO,
  bounding browser-driven guessing of preset `--auth user:pw` credentials;
  the credential check itself is unchanged.
