---
type: adr
area: surface
date: 2026-09-26
deciders: CypherLite maintainers
name: 0109-bolt-tls
status: accepted
tags:
  - auth
  - bolt
title: Bolt TLS and plaintext policy
REFERENCES:
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
---

# 0109. Bolt TLS and plaintext policy

## Context

`cypherlite-bolt` is the optional remote transport ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)). Today it is
plaintext-only: `serve` binds a raw `tokio::net::TcpListener` at
`surfaces/cypherlite-bolt/src/main.rs:289` and hands every accepted socket to
`handle_conn` with no transport security. The crate has no TLS dependency, and
`--bind` accepts any address — including a non-loopback interface — without a
warning, so an operator can expose a read/write graph over the network in the
clear by accident.

Linear LABS-111, tracked as OPLANE_REQ-00262968 (High), asks the Bolt server to
support TLS, optionally require client certificates, and stop silently exposing
plaintext on non-loopback binds.

The decisions below were settled by the maintainer before this ADR was written;
this record states them rather than reopening them.

## Decision

Add TLS to `cypherlite-bolt` with **`tokio-rustls`** (rustls 0.23.45) and
**`rustls-pemfile`** (2.2.0) for PEM loading. Both versions are already in
`Cargo.lock` via `ureq` and `neo4rs`, so this adds no new dependency family.

### Flags

- `--tls-cert <PATH>` — PEM certificate chain to present.
- `--tls-key <PATH>` — PEM private key for `--tls-cert`.
- `--tls-client-ca <PATH>` — PEM CA bundle used to verify client certificates;
  its presence enables mTLS.
- `--allow-plaintext` — explicit opt-in to plaintext on a non-loopback bind.

### One port, first-byte peek

A single listener serves both `bolt://` and `bolt+s://`. On each accepted
connection, `cypherlite-bolt` peeks the first byte before choosing a path:

- `0x16` (TLS `ClientHello` record) → run the TLS handshake.
- `0x60` (Bolt magic preamble) → plaintext Bolt.
- anything else → close the connection.

### Plaintext policy

- Loopback binds allow plaintext silently.
- A non-loopback bind **refuses to start** when TLS is unconfigured and
  `--allow-plaintext` is absent.
- With TLS configured, a non-loopback connection that takes the plaintext path
  is refused per connection unless `--allow-plaintext` is set.
- `--allow-plaintext` prints a prominent stderr warning naming the bind
  address.

### mTLS

When `--tls-client-ca` is set, the server builds a rustls
**`WebPkiClientVerifier`** from those roots; TLS clients must then present a
certificate signed by that CA.

### Ciphers, versions, reload

- Cipher suites and TLS versions are rustls defaults only; there are no knobs.
- Certificates and keys reload only on **process restart**.

## Alternatives considered

- **TLS-only port (reject plaintext entirely).** Rejected: the maintainer chose
  one port that serves both `bolt://` and `bolt+s://` so existing clients can
  migrate gradually.
- **Separate TLS listener port.** Rejected: it means two listeners to operate
  and document for no benefit over the first-byte peek.
- **`native-tls`.** Rejected: it pulls in a platform/OpenSSL native dependency,
  against the embedded, cross-platform story; rustls is pure Rust and already in
  the tree.
- **Deferring mTLS to a follow-up.** Rejected: the maintainer chose to include
  mTLS now rather than ship TLS without client-certificate support.

## Consequences

- `cypherlite-bolt` gains `tokio-rustls` and `rustls-pemfile` as normal
  dependencies (both already in `Cargo.lock`).
- The single-port peek keeps one endpoint and one documented bind for both
  schemes; a malformed first byte closes the connection instead of hanging.
- Operators who previously exposed plaintext on a non-loopback bind must now
  either configure TLS or pass `--allow-plaintext` and accept the warning.
- mTLS verification is available immediately via `--tls-client-ca`; rotating a
  certificate or CA still requires a restart.
- Tests and documentation must cover both peek paths, the non-loopback startup
  refusal, per-connection plaintext refusal, and mTLS.
