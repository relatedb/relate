---
type: adr
area: cypher
date: 2026-10-07
deciders: Oskar Hane, CypherLite maintainers
name: 0182-unicode-identifiers
status: proposed
tags:
  - dialect
  - parser
title: "Unicode identifiers: the CYPHER 25 rule with a checked-in Unicode 13.0.0 table"
REFERENCES:
  - "[[adr/0002-cypher-25-semantic-target]]"
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0169-keywords-as-label-and-type-names]]"
  - "[[adr/0268-proptest-property-testing]]"
---

# 0182. Unicode identifiers: the CYPHER 25 rule with a checked-in Unicode 13.0.0 table

## Context

LABS-188: Neo4j accepts non-ASCII characters in unquoted identifiers;
CypherLite's lexer accepts only ASCII. The identifier predicates in
`cypherlite/cypherlite-parser/src/lexer.rs` are byte predicates:

```rust
fn is_ident_start(ch: u8) -> bool { ch == b'_' || ch.is_ascii_alphabetic() }
fn is_ident_part(ch: u8) -> bool  { ch == b'_' || ch.is_ascii_alphanumeric() }
```

`read_identifier`, `read_parameter` and the main token dispatch loop over
bytes with these predicates. The part-2 LABS-188 change (commit `053507d6`)
already routes every non-ASCII, non-whitespace character that reaches the
fallback arm into one `InvalidUnicodeCharacter` error token; that is the
right shape for genuinely invalid characters, but today it also rejects
every character Neo4j accepts in an identifier.

The openCypher TCK does not pin the behavior (no pinned scenario exercises
a non-ASCII unquoted identifier), so the rule was established by probing
the pinned Neo4j 2026.09 container (`openjdk 25.0.4.1`): 333 queries run
under both `CYPHER 5` and `CYPHER 25`, each with its recorded outcome. The rows below
are a selection of the recorded `probe_table` rows (`query` and `cypher25`
fields); non-printing code points are written `\u{XXXX}` where the probe
stored the literal character. `42I06` is Neo4j's syntax error/access rule
violation for invalid input.

| Probe | Code point, position | CYPHER 25 |
| --- | --- | --- |
| `RETURN 1 AS Éx` | U+00C9 É (Lu), start | ok |
| `RETURN 1 AS xÉ` | U+00C9 É, part | ok |
| `RETURN 1 AS \u{0301}x` | U+0301 combining acute (Mn), start | 42I06 |
| `RETURN 1 AS x\u{0301}` | U+0301 combining acute, part | ok |
| `RETURN 1 AS 0x` | U+0030 0 (Nd), start | 42I06 |
| `RETURN 1 AS x0` | U+0030 0, part | ok |
| `RETURN 1 AS €x` | U+20AC € (Sc), start | 42I06 |
| `RETURN 1 AS x€` | U+20AC €, part | ok |
| `RETURN 1 AS x¢` | U+00A2 ¢ (Sc ∩ Pattern_Syntax), part | 42I06 |
| `RETURN 1 AS ⸯx` | U+2E2F ⸯ (Lm ∩ Pattern_Syntax), start | 42I06 |
| `RETURN 1 AS ‿x` | U+203F ‿ (Pc), start | ok |
| `RETURN 1 AS ࢾx` | U+08BE ࢾ (Lo, Unicode 13.0 addition), start | ok |
| `RETURN 1 AS ࡰx` | U+0870 ࡰ (Lo, Unicode 14.0 addition), start | 42I06 |
| `RETURN 1 AS 𐐀x` | U+10400 𐐀 (Lu, supplementary plane), start | 42I06 |
| `RETURN 1 AS x$` | U+0024 $ (Sc ∩ Pattern_Syntax), part | 42I06 |
| `RETURN 1 AS ℘x` | U+2118 ℘ (Sm, Other_ID_Start), start | 42I06 |
| `RETURN 1 AS ²x` | U+00B2 ² (No), start | 42I06 |
| `RETURN 1 AS ſKIP` | U+017F ſ (Ll), alias | ok |
| `RETURN 1 ſKIP 0` | U+017F ſ, keyword lookalike | 42I06 |
| `RETURN $名前 AS v` | parameter name | ok |

The rows imply the exact rule, with no ad-hoc exceptions:

- **identifier start** = BMP only (`code_point <= U+FFFF`) AND
  `General_Category ∈ {Lu, Ll, Lt, Lm, Lo, Nl, Pc}` AND NOT
  `Pattern_Syntax`;
- **identifier part** = BMP only AND `General_Category ∈ {Lu, Ll, Lt, Lm,
  Lo, Nl, Pc, Mn, Mc, Nd, Sc}` AND NOT `Pattern_Syntax`.

The tables are Unicode **13.0.0**, not the Java 25 runtime's Unicode 16.0
tables: the Unicode-13.0 additions U+08BE, U+9FFC and U+A7BA are accepted,
and the Unicode-14.0 additions U+0870, U+08C8, U+2C2F, U+9FFD, U+20C0,
U+0898, U+1715 and U+180F are rejected. The classes are **not** UAX #31
`ID_Start`/`ID_Continue`: U+203F (Pc) is a valid start (XID_Start excludes
Pc), U+2118/U+212E/U+309B/U+309C (`Other_ID_Start`) are rejected, Sc is a
valid later character, and supplementary-plane letters are rejected while
remaining usable inside string literals and backtick-quoted identifiers.

The identifier token is also what keyword recognition consumes: `ſKIP`
(U+017F) lexes as one identifier and is not case-folded to `SKIP`, so
keyword matching stays exact-spelling on the token (the label/type-name
predicate work of [ADR-0169](./0169-keywords-as-label-and-type-names.md)
operates on tokens and is unaffected). Per
[ADR-0002](./0002-cypher-25-semantic-target.md), live Neo4j behavior under
`CYPHER 25` is authoritative where sources disagree, so the `CYPHER 5`
behavior seen in the same probe (U+2E2F at start, `$` and the extra part
characters, U+0085 as an identifier part) is not a second rule to support.

## Decision

**1. The predicates.** The parser lexer implements exactly these two
`char` predicates, over Unicode 13.0.0 `General_Category` and
`Pattern_Syntax`:

- `is_identifier_start(c)`: `c as u32 <= 0xFFFF` AND `GC(c) ∈ {Lu, Ll,
  Lt, Lm, Lo, Nl, Pc}` AND NOT `Pattern_Syntax(c)`.
- `is_identifier_part(c)`: `c as u32 <= 0xFFFF` AND `GC(c) ∈ {Lu, Ll,
  Lt, Lm, Lo, Nl, Pc, Mn, Mc, Nd, Sc}` AND NOT `Pattern_Syntax(c)`.

There are no exceptions (`start_exceptions` and `part_exceptions` are
empty), and supplementary-plane code points are never identifier
characters.

**2. Only the CYPHER 25 rule.** There is no version switch and no
`CYPHER 5` compatibility mode. The `CYPHER 5`-only entries of the probe's
`cypher5_differences` are not replicated:

- U+2E2F accepted at start (CYPHER 25 rejects it);
- the extra part characters: U+0000–U+0008, U+000E–U+001B,
  U+007F–U+009F, `$`, U+00A2–U+00A5, U+00AD, U+0600–U+0605, U+061C,
  U+06DD, U+070F, U+08E2, U+180E, U+200B–U+200F, U+202A–U+202E,
  U+2060–U+2064, U+2066–U+206F, U+FEFF, U+FFF9–U+FFFB;
- U+0085 (NEL) as an identifier part — CypherLite lexes it as whitespace,
  as CYPHER 25 does;
- all-`Sc` acceptance — CYPHER 25 accepts `Sc` only outside
  `Pattern_Syntax` (U+20AC ok; U+00A2–U+00A5 rejected), and never at
  start.

The two remaining `cypher5_differences` entries — the Unicode 13.0.0
tables (not Java's Unicode 16.0) and the BMP-only rejection of
supplementary-plane characters — hold in both versions and are replicated.

**3. Mechanism: a checked-in generated table.** A stdlib-only Python
generator, `scripts/gen-unicode-identifier-tables.py`, is checked in. It
reads `UnicodeData.txt` and `PropList.txt` from
`https://www.unicode.org/Public/13.0.0/ucd/` (the SHA-256 of both inputs
is pinned in the script), applies the two predicates, and writes the
data-only module
`cypherlite/cypherlite-parser/src/unicode_identifier/tables.rs`:

- `pub const UNICODE_VERSION: (u8, u8, u8) = (13, 0, 0);`
- two sorted, inclusive BMP range tables `&[(u16, u16)]` — 383 ranges
  for the start predicate and 436 for the part predicate (819 ranges,
  3,276 bytes of static data);
- a header recording the source URLs, the input hashes and the exact
  regeneration command.

The `is_identifier_start(char)` / `is_identifier_part(char)` predicates
and their `partition_point` range lookup are hand-written in the parent
module `cypherlite/cypherlite-parser/src/unicode_identifier.rs`. The ASCII
code points are decided there (`[A-Za-z_]` / `[A-Za-z0-9_]`) before the
tables are consulted, which is both the fast path and the source of truth
for ASCII; only a non-ASCII `char` is looked up in the generated tables.
`read_identifier`, `read_parameter` and the dispatch arm use the `char`
predicates, and the fallback keeps emitting `InvalidUnicodeCharacter`.

Per AGENTS.md R6 this is not a heavy dependency: the shipped engine adds
**no dependency at all**. The artifact is two small static arrays in an
engine crate, and the generator is out-of-tree tooling — not built, not
linked, not in the resolved workspace graph — so [ADR-0103](./0103-engine-surfaces-testing-layout.md) R1 (engine crates
depend only on engine crates) is unaffected. The crate's *test build* gains
one external dev-dependency: `sha2 = { workspace = true }` under
`cypherlite/cypherlite-parser`'s `[dev-dependencies]`, the workspace-pinned
canonical SHA-256 (0.10) that the drift guard uses to pin the generated
file. It is dev-only — never compiled into the shipped engine or a release
artifact — and light (no native/C++ build, no async runtime, no networking),
the same posture as `cypherlite-storage`'s `tempfile` / `proptest`
([ADR-0268](./0268-proptest-property-testing.md)). Regeneration is a manual,
reviewed step (CI does not fetch the UCD); a unit test pins `UNICODE_VERSION`
and the probe's boundary code points.

## Alternatives considered

- **UAX #31 `XID_Start`/`XID_Continue` via the `unicode-ident` crate
  (1.0.18).** Rejected: it is a different predicate. `XID_Start` /
  `XID_Continue` are NFKC-closed derivations of `ID_Start` /
  `ID_Continue`, which add `Other_ID_Start` (U+1885, U+1886, U+2118,
  U+212E, U+309B, U+309C) and `Other_ID_Continue` (U+00B7, U+0387,
  U+1369, U+19DA), exclude `Sc`, exclude `Pc` from the start set, and
  include supplementary-plane characters. Every one of those diverges
  from the probe: U+2118, U+212E, U+309B, U+309C, U+00B7, U+0387,
  U+1369, U+19DA and U+10400 are all `42I06` in CYPHER 25, while U+20AC
  is `ok` as a part and U+203F is `ok` as a start. The crate's 1.0.18
  tables are generated from Unicode 16.0.0 (its `tables.rs` header), while
  Neo4j's classes are 13.0.0 (U+0870 rejected). It cannot express the
  rule.
- **std `char::is_alphabetic` / `is_alphanumeric`.** Rejected: neither
  predicate can express the rule. `is_alphabetic` is the Alphabetic
  property (`L* + Nl + Other_Alphabetic`), so it accepts combining marks
  such as U+0345 that Neo4j rejects as a start, and it accepts U+2E2F
  (Lm ∩ Pattern_Syntax) that CYPHER 25 rejects. `is_alphanumeric` adds
  General_Category N, so it accepts U+00B2 (No) that Neo4j rejects in
  both positions, while still rejecting `Sc` (U+20AC) that Neo4j accepts
  as a part. Both are position-agnostic, have no `Pattern_Syntax`
  exclusion and no BMP restriction, and Rust 1.88's std tables are
  Unicode 16.0.0 (`UNICODE_VERSION = (16, 0, 0)`), not 13.0.0.
- **Keep ASCII-only identifiers and require backticks for everything
  else.** Rejected: it diverges from Neo4j in exactly the direction
  LABS-188 exists to fix. The probe shows Neo4j accepts non-ASCII in
  every unquoted identifier position — variable, alias, label,
  relationship type, property key, map key and parameter (`RETURN $名前
  AS v` and `MATCH (n:名前) RETURN 1 AS x` are `ok`) — so real queries
  such as `WITH 1 AS café RETURN café AS x` would keep failing, and valid
  non-ASCII identifiers would keep lexing as `InvalidUnicodeCharacter`.
- **The `unicode-general-category` crate (0.4.0, the release whose
  README states Unicode 13.0 data; 1.0.0/1.1.0 track Unicode 16.0).**
  Rejected: it exposes `General_Category` only, and `Pattern_Syntax` is a
  binary property it does not carry (nor does `unicode-properties`
  0.1.4, which exposes General_Category + Emoji). The crate route
  therefore still needs a checked-in `Pattern_Syntax` table — two
  mechanisms for one predicate — plus a dependency whose Unicode version
  is implicit in its release history rather than explicit in the repo.
  Its lookup structure is ~45 KiB for all 30 categories; the generated
  table stores the two membership bits in ~3.3 KiB. Picking 1.1.0
  instead would silently accept the Unicode 14+ letters (U+0870, U+08C8,
  …) that the probe rejects.

## Consequences

- `is_ident_start`/`is_ident_part` in
  `cypherlite/cypherlite-parser/src/lexer.rs` are gone: the lexer calls the
  hand-written `is_identifier_start`/`is_identifier_part` predicates in
  `cypherlite/cypherlite-parser/src/unicode_identifier.rs` directly, and the
  generated data lives in
  `cypherlite/cypherlite-parser/src/unicode_identifier/tables.rs`. The ASCII
  path stays a byte-level direct match (the predicates decide ASCII before
  consulting the tables), and only non-ASCII decodes via `decode_char` and
  looks the code point up in the tables. `read_identifier`, `read_parameter`
  and the main dispatch use the predicates.
- `InvalidUnicodeCharacter` still covers every non-ASCII non-whitespace
  character that is not an identifier start: a decoded character that is
  neither a valid start at a token start nor consumed as a part is
  reported exactly as the part-2 change does today — one error token
  carrying the decoded character and the detail. The Mathematical3 [1]
  allowlist entry and the strict class+detail matcher are unaffected.
- No normalization: identifiers are compared code-point-exact.
  `WITH 1 AS café RETURN café AS x` is ok, but binding the precomposed
  form and referencing the decomposed form (`e` + U+0301), or the
  reverse, fails with `42N62 Variable ... not defined` — they are
  different variables. NFKC is not applied either (`ﬁ` ≠ `fi`, fullwidth
  `ａ` ≠ `a`), and comparison is case-sensitive (`café` ≠ `CAFÉ`).
- Numbers glued to identifier characters stay a lexer-level rejection:
  `RETURN 1é` (and `RETURN 1é AS x`, `RETURN 1é + 1 AS x`) is `42I07
  invalid integer literal. The given decimal integer literal '1é' is
  invalid.` in both versions, like the ASCII controls `1x` and `1e`. The
  number reader greedily consumes following identifier characters into
  the numeric token and rejects the malformed literal.
- Keyword lookalikes never become keywords: `ſKIP` lexes as one
  identifier and is not case-folded to `SKIP`, so `RETURN 1 ſKIP 0` is
  `42I06` while `RETURN 1 AS ſKIP` and `WITH 1 AS ſKIP RETURN ſKIP AS x`
  are ok.
- Parameters: `$` is the parameter sigil and is never part of an
  identifier in CYPHER 25 (`RETURN 1 AS x$`, `RETURN 1 AS a$b`,
  `RETURN 1 AS x$y` are all `42I06`). Parameter names follow the same
  unquoted-identifier rule as variables, labels and property keys:
  `RETURN $名前 AS v` and `RETURN $café AS v` succeed with the parameter
  supplied via `cypher-shell -P`.
- The formatter keeps backticking non-ASCII names: `cypher_identifier`
  in `surfaces/cypherlite-formatter/src/lib.rs` (via
  `is_bare_identifier`) and `vis_ident` in
  `cypherlite/cypherlite-executor/src/graph_type_visualize.rs` treat only
  ASCII `[A-Za-z_][A-Za-z0-9_]*` as bare, so a non-ASCII name is still
  emitted as `` `café` ``. That stays valid and round-trips; no change is
  needed there.
- The implementation change documents the rule in
  `docs/reference/cypher-syntax.md` and updates the CHANGELOG. The TCK
  allowlist (3892 of 3893, one documented skip) and the CIP-eval
  baselines must not regress; no pinned TCK scenario exercises a non-ASCII
  unquoted identifier today, and any scenario that starts passing is added
  to the allowlist per AGENTS.md.
