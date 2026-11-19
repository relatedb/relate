---
type: adr
area: surface
date: 2026-09-27
deciders: CypherLite maintainers
name: 0122-pull-auth-hardening
status: proposed
tags:
  - load
  - security
title: "Pull credential hardening: plaintext refusal and env-file warnings"
REFERENCES:
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0109-bolt-tls]]"
SUPERSEDES:
  - "[[adr/0260-trust-boundary-error-sanitization]]"
---

# 0122. Pull credential hardening: plaintext refusal and env-file warnings

## Context

`cypherlite pull` materializes a remote Neo4j/Aura subgraph into a local
`Database` ([ADR-0080](./0080-cli-pull-command.md)). Credentials come from a `KEY=VALUE` env file passed with
`--from FILE` (or from the process environment when `--from` is omitted) and are
sent to the Query API as HTTP Basic auth. [ADR-0080](./0080-cli-pull-command.md) sets the transport to HTTP
Query API v2 and maps `neo4j://host:7687` / `bolt://host:7687` to
`http://host:7474`.

Two credential-exposure paths follow from that shape:

- An explicit `http://host…` origin, or a non-loopback `neo4j://`/`bolt://`
  host, sends the Basic-auth header in cleartext over the network. A user may
  not realize that a remote endpoint is plaintext.
- `--from FILE` is read from disk without checking its permissions. A
  group- or world-readable env file exposes `NEO4J_PASSWORD` to any local
  account, which is common when CI or containers create files with a default
  `umask` (`0644`).

`cypherlite-bolt` already has an explicit plaintext opt-in, `--allow-plaintext`
([ADR-0109](./0109-bolt-tls.md)): loopback is allowed silently, and a non-loopback plaintext use
requires the flag and prints a prominent warning. Pull should follow the same
policy and the same flag spelling rather than invent a second vocabulary.

Linear LABS-119, tracked as OPLANE_REQ-00262976, asks pull to refuse to send
credentials in cleartext off-loopback and to warn when the env file is too
permissive. The decisions below were settled by the maintainer before this ADR
was written; this record states them rather than reopening them.

## Decision

### Refuse Basic auth over plaintext off-loopback

`cypherlite pull` **refuses** to send Basic auth over a plaintext `http://`
origin to a **non-loopback** host unless `--allow-plaintext` is passed.

- The flag spelling is **`--allow-plaintext`**, identical to
 `cypherlite-bolt` ([ADR-0109](./0109-bolt-tls.md)).
- The refusal happens before any network call, prints an error to stderr that
  names `--allow-plaintext`, and exits non-zero.
- Loopback origins are allowed silently. `localhost`, `127.0.0.0/8`, and
  `::1` count as loopback.
- `https://` origins are never refused by this policy.
- When `--allow-plaintext` is passed, a prominent stderr warning names the
  plaintext host.

### Warn on permissive `--from` env files

When `--from FILE` is group- or world-readable, pull **warns** on stderr — it
does **not** refuse. The check is a Unix mode check (`mode & 0o077 != 0`) and is
a no-op on non-Unix hosts, where the mode is unavailable.

### Keep the loopback Query API mapping

The `neo4j://` / `bolt://` → `http://host:7474` mapping for loopback hosts is
kept as decided in [ADR-0080](./0080-cli-pull-command.md). It is **not** changed to `https`.

### Refuse userinfo in `NEO4J_URI` outright

`query_api_url` **rejects** any `NEO4J_URI` whose post-scheme remainder
contains `@` — at any position — with the fixed text `NEO4J_URI must not
contain userinfo; pass credentials via NEO4J_USERNAME/NEO4J_PASSWORD`. Pull
credentials come from `NEO4J_USERNAME`/`NEO4J_PASSWORD`, so userinfo in the
URI is never legitimate. The check runs on the raw URI before
`split_authority` truncates at the first `/`: a password containing `/`
(`http://user:p/ss@host:7687`) would otherwise cut the authority to `user:p`
and silently parse the userinfo username as the host, dropping the real host
and persisting userinfo-derived bytes via `identity_source`. Pull URIs are
origins (`scheme://host[:port]`) and `query_api_url` rebuilds the path
itself, so no legitimate pull URI contains `@` at all. With userinfo refused
at this boundary, `uri_authority` no longer strips userinfo (the literal-`@`
`rsplit` was removed): one boundary rule, not strip-plus-parse.

### Validate the URI host; never echo the raw `NEO4J_URI`

`query_api_url` **validates** the parsed host against the DNS / IP-literal
charset (`[A-Za-z0-9.-]`, plus hex/`:` inside bracketed IPv6 literals) and
rejects anything outside it with the fixed text `NEO4J_URI has an invalid
host`. The missing-scheme, missing-host, and unsupported-scheme errors are
likewise fixed text (`NEO4J_URI missing scheme` / `NEO4J_URI missing host` /
`unsupported NEO4J_URI scheme (use neo4j/bolt/http/https or +s/+ssc)`). No
error arm interpolates the raw `NEO4J_URI`, the parsed host, the scheme
token, or any substring of any of them — the scheme token in particular is
never echoed, because `split_scheme` takes everything before the first
`://`, so a `user:pw@host://x` URI would put credentials in that token.

This replaces the earlier sanitize-the-error-text design. String redaction of
userinfo (`redact_uri_userinfo`) was abandoned after **three review rounds of
encoding bypasses**: a literal-`@` scan missed `user:p/ss@host` (password
hiding the `@` past the authority boundary) and `%40`-encoded separators; the
encoded-`@` fix was in turn bypassed by double-encoding (`%2540`) and by
`uri_authority` parsing `http://user%3Asecretpw%40host` with the encoded
credential **as the host** — a value that was then echoed raw in the
cleartext-refusal error, the opt-in warning, and the Io error arms, and
persisted to disk via `identity_source`. Validation is the robust rule
because a real hostname can never contain `%` or `@`: every encoding probe is
refused at the boundary instead of pursued with ever-wider string surgery.
Userinfo at a **literal** `@` was initially still stripped for URL
construction; a later review round showed the strip was itself unsound (a `/`
in the password hides the `@` past the authority boundary), so userinfo URIs
are now **rejected outright** at the `query_api_url` boundary and the
stripping code was removed.

## Alternatives considered

- **Refuse on permissive env files.** Rejected: it breaks legitimate CI and
  container setups that create the file with a default `umask`, and the issue
  only requires a warning — its test bullet states “a 0644 env file warns”.
- **Map `neo4j://`/`bolt://` to `https` by default.** Rejected: it breaks the
  common local Neo4j dev flow, whose Query API is http on 7474, and the
  off-loopback refusal already closes the credential-exposure hole.
- **A distinct flag spelling such as `--allow-cleartext-auth`.** Rejected: it
  splits the vocabulary for the same opt-in; consistency with the shipped
 `cypherlite-bolt` flag ([ADR-0109](./0109-bolt-tls.md)) matters more than wording preference.
- **Silently allow plaintext everywhere (status quo).** Rejected: it exposes
  credentials whenever a user points pull at a non-loopback `http` endpoint.
- **Refuse plaintext even on loopback.** Rejected: loopback never leaves the
  host, and the local Neo4j default is http on 7474; refusing it would block
  the primary use case for no security gain.
- **Keep sanitizing error text with `redact_uri_userinfo`.** Rejected: three
  review rounds each found a new encoding bypass (literal-only `@` scan,
  `%40` handling, then double-encoding `%2540` and encoded-userinfo-as-host).
  String surgery on attacker-influenced input is unbounded; validating the
  host against a charset a real hostname must satisfy is a single,
  exhaustively testable rule.

## Consequences

- `--help`, `docs/api/cli/spec.md`, and the [ADR-0080](./0080-cli-pull-command.md) decision text gain the
  `--allow-plaintext` flag and the plaintext policy in the same change.
- Off-loopback plaintext pulls that previously worked now fail non-zero until
  the user passes `--allow-plaintext`; the error names the flag.
- A `0644` `--from` env file prints a warning but still pulls, so existing CI
  keeps working.
- The env-file check must be `#[cfg(unix)]`-guarded with a non-Unix no-op, so
  Windows and wasm-less builds compile and behave unchanged.
- Tests must cover: loopback plaintext allowed, off-loopback plaintext refused
  with the flag named, `--allow-plaintext` opting in with a warning, and a
  `0644` env file warning without failing.
- Tests must also cover: userinfo rejection for literal-`@` URIs (including
  passwords containing `/` in both `http://` and `neo4j://` shapes), the
  fixed-text missing-scheme/missing-host and
  unsupported-scheme errors (asserting neither credential, scheme token, nor
  raw URI appears), host-charset rejection
  of single- and double-encoded userinfo probes, acceptance of
  `localhost`/IPv4/bracketed-IPv6/DNS hosts, and `identity_source` refusing an
  encoded-userinfo URI so it can never reach the on-disk pull-identity map.
