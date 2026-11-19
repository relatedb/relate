---
type: adr
area: surface
date: 2026-09-23
deciders: CypherLite maintainers
name: 0083-borrowed-bolt-input-buffer
status: proposed
tags:
  - bolt
  - security
title: Borrow Bolt input from one bounded payload buffer
REFERENCES:
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0082-bound-bolt-decoded-value-count]]"
---

# 0083. Borrow Bolt input from one bounded payload buffer

## Context

[ADR-0057](./0057-embedded-c-abi-and-bolt.md) makes Bolt an optional remote transport. Incoming messages can be
decoded before authentication, so avoidable copies consume resources on an
untrusted path. `protocol::read_message` currently allocates a temporary
`Vec<u8>` for each chunk and copies it into a payload `Vec<u8>`.
`packstream::decode` then copies the entire payload with
`Bytes::copy_from_slice`; decoding a string or byte field copies its bytes
again through an intermediate `Bytes` before constructing the owned `Pack`
value.

The 16,777,216-byte chunked-message payload limit, 128-container depth limit,
and [ADR-0082](./0082-bound-bolt-decoded-value-count.md)'s 262,144-decoded-value budget already bound distinct aspects of
incoming messages. They do not remove these copies. The Bolt crate uses
`bytes::Buf` and `bytes::Bytes` for decoding; storage separately uses
`bytes::Bytes` for Parquet and depends on the workspace `bytes` entry.

## Decision

Use a single owned, bounded `Vec<u8>` as the incoming Bolt message payload.
Before accepting each nonempty chunk, check that its declared length fits
within the remaining 16,777,216-byte payload budget. Only then grow the
payload and `read_exact` directly into its new suffix. A truncated read fails
the message and drops the partial payload; no partially decoded message is
exposed.

Decode from a borrowed `&[u8]` over that payload using a checked slice cursor
for markers, length prefixes, fixed-width fields, strings, and byte fields.
Borrow field slices during parsing; materialize one final owned `String` or
`Pack::Bytes(Vec<u8>)` per returned string/byte value, without a full-message
copy or intermediate owned field buffer. Other `Pack` values continue to own
their decoded data, and no decoded value borrows from the payload after
decoding returns.

Keep the same Bolt wire format, marker and truncated-input/invalid-UTF-8 error
behavior, connection-local failure, exact payload boundary, 128-container
depth, 262,144-value budget ([ADR-0082](./0082-bound-bolt-decoded-value-count.md)), count-versus-remaining-byte checks,
incremental collection growth, and map32/struct-width behavior. This changes
buffer ownership and the number of copies, not the protocol or its resource
limits.

Remove the direct `bytes` dependency from
`crates/cypherlite-bolt/Cargo.toml` when implementing the cursor. Retain the
root workspace `bytes` dependency and `cypherlite-storage`'s dependency and
usage; this decision does not remove `bytes` from the workspace.

## Alternatives considered

- **Keep temporary chunk vectors and `Bytes::copy_from_slice`.** Rejected:
  every incoming chunk is copied into the payload and the whole payload is
  copied again before decoding, even for a short invalid pre-auth message.
- **Move the payload into `Bytes` after reading, but retain separate chunk
  vectors.** Rejected: this could avoid the whole-payload copy but leaves the
  per-chunk allocation/copy and Bolt's direct `bytes` dependency; it does not
  establish one bounded buffer from socket to decoder.
- **Retain chunks separately and return borrowed string/byte `Pack` fields.**
  Rejected: field spans may cross chunk boundaries, and lifetime-bearing
  decoded values would complicate message ownership and downstream conversion
  while retaining many chunk allocations. Final owned fields are already
  required by the current `Pack` API.

## Consequences

- The maximum message payload remains 16 MiB, but receiving and parsing it no
  longer require a second full-message allocation or temporary chunk buffers.
  Owned string/byte results still allocate once each, and the bounded payload
  stays live while decoded values are constructed.
- Bounds checks move to the direct-append and slice-consumption boundaries;
  tests must cover truncated chunks and fields, UTF-8, the exact and exceeded
  message limit across multiple chunks, value/depth limits, and the existing
  connection-local error behavior.
- Bolt no longer needs `bytes` directly. Workspace and storage dependencies
 on `bytes` remain in place. [ADR-0057](./0057-embedded-c-abi-and-bolt.md)'s transport role and [ADR-0082](./0082-bound-bolt-decoded-value-count.md)'s decoded
  value budget remain unchanged.
