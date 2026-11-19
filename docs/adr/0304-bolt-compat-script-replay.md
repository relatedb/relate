---
type: adr
area: surface
claim: 6be6231e21d460a6e1364167cda6fed3d8829fca
name: 0304-bolt-compat-script-replay
status: proposed
tags:
  - bolt
  - conformance
title: Bolt compatibility corpus as boltkit `.bolt` scripts, replayed against the real server
REFERENCES:
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
---

# ADR-0304: Bolt compatibility corpus as boltkit `.bolt` scripts, replayed against the real server

## Context

`cypherlite-bolt` is CypherLite's remote transport ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)). It is a
transport, not a Neo4j feature-parity promise, but official Neo4j drivers
and tools must be able to talk to it. Its tests today come in three kinds:

- Unit tests in `src/` that drive the session in process.
- Hand-written end-to-end roots (`e2e_auth`, `e2e_browser_sidebar`,
  `e2e_websocket`, `concurrency`). These spawn the real binary through
  `tests/common` and build each exchange in Rust.
- The official Python driver smoke test in the CI smoke leg.

None of these is a compatibility corpus, meaning a list of wire
conversations someone can read, review and extend without reading Rust.
LABS-323 asks for one. In the e2e roots, each conversation is imperative
code built from `Pack` constructors. Reviewing a case means reading that
code, and adding one means writing more.

Neo4j's driver teams solved the mirror problem with
[boltkit](https://github.com/neo4j-drivers/boltkit). Its stub server plays
scripted conversations, `.bolt` files, to test a driver. The grammar is
defined in `boltkit/server/scripting.py` (branch 4.3, `687a479`, last
changed December 2021). It is compact:

- `!:` meta lines.
- `C:` and `S:` lines, each a message name followed by JSON-encoded fields.
- Continuation lines with no role, which inherit the previous role.
- The `<EXIT>`, `<RAW>` and `<NOOP>` commands.

boltkit also ships a body of scripts, such as `test/scripts/v4.0/*.bolt`.
But boltkit tests clients. In its scripts the stub plays the server, so
neither boltkit nor its scripts can check a server.

## Decision

1. **The Bolt compatibility corpus is boltkit-format `.bolt` scripts, with
   the roles turned around.** The test harness is the client. It sends a
   script's `C:` lines to a real `cypherlite-bolt` process and checks the
   server's replies against the `S:` lines. Scripts live under
   `surfaces/cypherlite-bolt/tests/compat/scripts/`, and every `*.bolt`
   file found there recursively is part of the corpus. Each script runs
   against its own server, started through the `tests/common`
   `start_server`. The default flags are `--uri :memory: --auth none`, and
   a `!: SERVER_ARGS` line replaces them.
2. **The grammar follows boltkit where it applies to a client, and is
   strict everywhere else.** Kept from boltkit: `!:`, `C:` and `S:` lines,
   continuation lines, JSON fields, the Bolt 4.4 message names, `0xNN` for
   any tag, and the `<RAW>`, `<NOOP>` and `<EXIT>` commands. Changed or
   added for the client side:
   - `!: BOLT M.m` is the handshake the harness sends: the magic
     `60 60 B0 17` plus a single `00 00 m M` proposal, padded with zeros to
     four proposals. The harness then expects `00 00 m M` back. Without the
     line, the handshake proposes and expects `4.4`.
   - `!: HANDSHAKE <hex>` sets the expected 4-byte reply.
   - `!: AUTO HELLO` sends HELLO `{user_agent, scheme: "none"}` first and
     expects SUCCESS.
   - `!: AUTO GOODBYE` sends GOODBYE last and expects the server to close
     the connection.
   - `!: SERVER_ARGS` is new.
   - If a script's first `C:` line is a `<RAW>`, the script writes its own
     handshake and no automatic one is sent.
   - `C: <NOOP>` sends an empty chunk (`00 00`).
   - `S: <RAW>` expects exactly the given bytes.
   - `S: <EXIT>` expects the server to close the connection. End of stream
     and a reset both count.
   - A JSON integer is an Int. A number with a fraction or an exponent is a
     Float. `{"#NN": [fields]}` is a structure with hex tag `NN`.

   Anything else is a parse error that names `path:line`. That includes an
   unknown directive or `AUTO` value (boltkit's `!: PORT`, `!: AUTO RESET`),
   an unknown command (`<SLEEP>`) or message name, malformed JSON, a
   duplicate directive, a directive after the first `C:`/`S:` line, and a
   directive that contradicts a leading `<RAW>`. boltkit keeps text it
   cannot parse as JSON as a string field. The harness drops that lenient
   fallback.
3. **Matching rules.**
   - Tags must be equal.
   - An expected map matches when every listed key is present with a
     matching value. Extra server keys are allowed, and the string `"*"`
     as a map value matches any value.
   - Lists and structure fields match element by element under the same
     rules, and must have the same length.
   - Scalars match exactly, and an Int never equals a Float.

   Summary metadata (`type`, `qid`, `t_first`, `bookmark`, `db`) differs
   from Neo4j's and changes as the server is fixed. Subset maps let each
   script assert only what its case is about.
4. **Failures name the script line.** A mismatch reports the script path
   and line, the line as written, the expected value, and the decoded
   actual reply, written in the script notation. Every read and write has
   a 10 s timeout, so a server that never answers fails the step instead
   of hanging the suite. The harness is the Rust integration-test target
   `bolt_compat` (`tests/bolt_compat.rs`, with the `tests/compat/` module
   tree). It reuses `tests/common` and the crate's own `packstream` and
   `protocol::read_message`, and adds no dependency. One test replays
   every script, four at a time, and panics listing every failure.
5. **CI runs the corpus in the bolt e2e leg**, which gains
   `--test bolt_compat`.
6. **boltkit's `test/scripts/v4.0` scripts are the seed.** `return_1`,
   `return_1_as_x`, `return_1_as_x_explicit` and `return_5_records` are
   ported to Bolt 4.4. Each carries a header that names its boltkit
   source and lists what was adapted. `tests/compat/README.md` is the
   script reference.

## Alternatives considered

- **Plain Rust e2e tests, one `#[test]` per conversation.** This is the
  pattern of the existing e2e roots. Rejected: each conversation stays
  imperative code built from `Pack` constructors. Review means reading
  Rust, and every new case needs code and a rebuild. The cases also cannot
  be compared line by line with boltkit's scripts or with a Neo4j
  transcript. The script harness keeps Rust for the machinery only.
- **Running boltkit's Python suite in CI.** Rejected: boltkit's scripts
  and stub server test drivers, so they cannot exercise `cypherlite-bolt`.
  The suite would also add a Python toolchain and a package last changed
  in 2021 to the bolt test legs, and its results would land outside
  nextest. The format is the valuable part, and this decision keeps it.
- **One `#[test]` per script.** Stable libtest has no dynamic test
  registration. Per-script tests would need either a hand-maintained list
  with a drift guard, or a custom test harness (`harness = false` plus a
  new dependency such as libtest-mimic). Rejected: dropping a file into
  `scripts/` should be the whole change, and one report that lists every
  failed `path:line` is as clear.
- **Exact matching of every reply.** Rejected: summary metadata the case
  does not care about would make every script fail when an unrelated key
  changes.
- **A new format (YAML or JSON cases).** Rejected: boltkit's format is
  already known to people who work on Bolt drivers, and boltkit's scripts
  port to it nearly verbatim.

## Consequences

- A new Bolt compatibility case is a `.bolt` file, and review is reading
  the script diff. A grammar change (a new directive or command, Bolt 5
  message names) updates the parser and `tests/compat/README.md` in the
  same change. A change to the matching rules supersedes this ADR.
- The harness is test-only, not a public surface, so it has no
  `docs/api` entry. `tests/compat/README.md` is its reference.
- A failing script fails the single corpus test. The panic lists every
  failing script line, but nextest cannot select a single script by name.
- Every script starts its own server process, so the corpus run time grows
  with the number of scripts, four scripts at a time.
- Porting already turned up one difference from boltkit's stub: the
  server names an unaliased `RETURN $x` column `$x`, as Neo4j does. The
  ported scripts expect `$x` and say why.
