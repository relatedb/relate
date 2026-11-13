# ADR-0303: Bolt version negotiation answers from the client's proposals

Status: Proposed
Claim: 87799a4f45cb7f2e3d4b227b57c491f4c7ff06d3

## Context

The Bolt server (ADR-0057) speaks Bolt 4.4 only. ADR-0126, ADR-0272 and
ADR-0177 all rest on that, and it stays.

The Bolt handshake is the magic `60 60 B0 17` followed by four big-endian
32-bit version proposals, in the client's order of preference. Each
proposal is laid out `00 RR mm MM`: `MM` is the major version, `mm` the
minor, and `RR` (Bolt 4.3+) the number of minors below `mm` the client also
accepts. So `00 02 04 04` offers 4.4, 4.3 and 4.2. Unused slots are
zero-filled. Since Bolt 5.7 a client can also send the manifest-v1
proposal `00 00 01 FF`. The server must reply with one version the client
proposed. When it supports none of them, it replies `00 00 00 00` and
closes the connection.

`negotiate_magic` in `surfaces/cypherlite-bolt/src/protocol.rs` reads the
magic and the 16 proposal bytes, ignores the proposals, and always writes
`00 00 04 04`. A client that never proposed 4.4 still gets a 4.4 reply:
for example a boltkit client (Bolt 4.3 at most), or a 5.x-only driver. The
client then either drops the connection with a confusing protocol error or
goes on to speak a version the server does not speak. The handshake spec
already defines the honest answer, `00 00 00 00`, and the server never
sends it.

The exchange runs under the ADR-0141 accept-time negotiation deadline. A
timeout aborts the connection without starting a session.

## Decision

The server reads the four proposals in client order. It replies with the
version it speaks only when the client proposed it, and otherwise replies
`00 00 00 00` and closes.

1. A proposal `00 RR mm MM` **covers Bolt 4.4** when `MM = 4` and
   `mm - RR <= 4 <= mm`. The subtraction is unsigned and saturates at 0,
   so a range wider than the minor reaches down to minor 0. The reserved
   first byte is not inspected.
2. The **first** covering proposal, in client order, decides the reply,
   which is `00 00 04 04`. The session then starts as before.
3. When **no** proposal covers 4.4, the reply is `00 00 00 00`. The
   connection then closes without reading anything further and without
   starting a session. The server logs one stderr line naming the
   proposals.
4. **Manifest proposals never match.** `00 00 01 FF` has `MM = 0xFF`, so
   the covering test rejects it. The server does not speak the manifest handshake,
   and a client that sends a manifest proposal next to `00 00 04 04` (or
   a range covering it) gets the plain 4.4 reply.
5. The selection is a pure function of the 16 proposal bytes. The
   handshake's outcome is typed: serve, version unsupported (answered
   `00 00 00 00`, close), or aborted (the ADR-0141 deadline). It is not
   encoded as an error string.
6. The ADR-0141 deadline is unchanged. The `00 00 00 00` reply is
   written inside the same negotiation phase as the 4.4 reply, under the
   same budget.

## Alternatives considered

- **Keep always answering `00 00 04 04`.** It is simpler, and every
  client that proposes 4.4 sees no difference. Rejected: it breaks the
  handshake contract for every other client. A client that did not
  propose 4.4 gets a version it never offered and fails later with a
  confusing error, where it should fail at once with a clear "no common
  version".
- **Echo the matching proposal verbatim (for example `00 02 04 04`).**
  Rejected: the reply is the chosen version, `00 00 mm MM`. The range byte
  only means something in a proposal, and drivers compare the reply to an
  exact version.
- **Answer the newest version inside a range, capped at 4.4, for any
  4.x proposal.** For example, answer `00 00 04 04` to `00 00 03 04`.
  Rejected: that client did not offer 4.4. A reply it did not propose is
  the very bug this ADR fixes.
- **Implement the manifest-v1 handshake (Bolt 5.7+).** Rejected for now.
  The manifest lists versions and capabilities, and only Bolt 5.x
  versions use it, which the server does not speak (ADR-0126, ADR-0272).
  Supporting it is part of a Bolt 5.x decision, not part of correcting
  4.4 negotiation.
- **Keep the connection open after `00 00 00 00` so the client can try a
  new handshake.** Rejected: the handshake spec says the server closes,
  and an open, unnegotiated connection would hold a `--max-connections`
  permit (ADR-0110) with no ADR-0141 deadline left to end it.

## Consequences

- Clients that propose 4.4, directly or through a range, are unaffected.
  That includes every Neo4j 4.4+/5.x/6.x driver, `cypher-shell`, and the
  repository's test helpers and loadtest clients, which all propose
  `00 00 04 04` in the first slot.
- A client that proposes no 4.4 now gets the spec's `00 00 00 00` and
  an immediate close. Before, it got a 4.4 reply it could not use.
  boltkit (max 4.3) and 5.x-only clients now fail at the handshake with
  the driver's standard "no supported protocol version" error.
- The negotiation rule is part of the Bolt client contract and is
  documented on the Protocol line of `docs/api/bolt/spec.md`.
- Adding another Bolt version later (for example, 5.x or the manifest
  handshake) changes the selection function and needs its own ADR. The
  typed handshake outcome already has the "unsupported" branch that such
  a change would extend.
