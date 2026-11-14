# ADR-0505: Parse-pipeline token and string representation — borrowed token text, owned AST strings, alloc-free keyword lookup, process-wide-seeded scope-map hasher

Status: Proposed

Claim: 7150b96d8c5c5eb958bf5ebf364a07c38b6034a4

## Context

Profiling the parse pipeline over a 2,103-statement corpus (1,819 openCypher
TCK statements plus 284 workload queries, ~204 B per statement) shows it is
allocation-bound: tokenize 1.9 µs / 87 allocations per statement, parser +
AST 4.0 µs / 176, parse + front-end lowering 9.4 µs / 295. Roughly 40–50 %
of all parse+lower CPU is libc heap traffic. On the sub-millisecond half of
the GBL SF0.1 latency suite (ADR-0243) parse + plan is 30–100 % of wall
time. The plan cache (LABS-790) removes that tax only for repeated
statement texts; this decision cuts it for every statement, first sight
and parameterized variants included. The two compose.

The measured sources this decision addresses:

1. **Token text.** `Token { literal: String }`
   (`cypherlite-parser/src/token.rs`) allocates for every token, punctuation
   included — `Lexer::emit` calls `literal.to_string()` even for `(`. The
   parser then clones the whole `Token` on every `next_token()`
   (`parser/mod.rs`), and `Parser::new` copies the entire source into an
   owned `String`.
2. **AST strings.** The AST carries owned `String`s for names, literals and
   operators; operator fields (`UnaryExpr::op`, `BinaryExpr::op`, …) are
   filled with freshly allocated copies of constants
   (`"+".to_string()` in `parser/case.rs`, `OR.to_string()`).
3. **Keyword lookup.** `lookup_keyword` runs `ident.to_ascii_uppercase()` —
   one heap copy per identifier — before its match cascade (5.5 % of lex
   self time).
4. **Front-end scope maps.** `pattern_vars`, `output_vars`, `call_scope`
   and the `validate/*` sets in `cypherlite-front` are std
   `HashMap`/`HashSet` with SipHash, rebuilt per statement: hashing is
   ≈10 % of lowering self time.

Facts that bound the choice:

- **Token lifetime.** Tokens outlive the `Lexer` (`tokenize_with_options`
  returns the `Vec<Token>`) but never outlive the input string: the only
  consumer is the `Parser`, built and dropped inside `parse_with_options`,
  `parse_expression_str` and the interpolation re-parse (whose input is a
  local `wrapped` string). No crate outside `cypherlite-parser` constructs
  or stores a `Token`; external users import only free functions
  (`lookup_keyword`, `quote_identifier`, `identifier_needs_quotes`,
  `lexer::skip_comment`).
- **Not all token text is input text.** A `TokenType::String` token
  carries the *decoded* literal (`\n`, `\uXXXX`, surrogate pairs resolved
  by `read_string`/`push_escape`); error tokens carry synthesized text,
  some fixed (`"unterminated string"`), some formatted
  (`"invalid unicode escape sequence: \u{…}"`,
  `"unexpected character: …"`). The literal rule (`lexer::skip_comment`,
  "the one spelling of the comment rule, shared by the lexer and by callers
  that re-read a literal token's text") and error messages both need the
  text itself, not just a position.
- **The AST outlives the input.** The parser AST is not only consumed and
  dropped by `cypherlite-front` lowering: `cypherlite-ir` embeds it
  (`Clause::Catalog(Box<cypherlite_parser::Clause>)`), the planner matches
  on it (`clause_kind.rs`, `namespaces.rs`, `names.rs`), and the executor
  and ontology crates parse and keep graph-type bodies
  (`parse_graph_type_body`). A borrowed AST would carry the input's
  lifetime into IR, the planner and every holder of a lowered `Query`.
- **Engine-internal only.** `cypherlite-parser` is not re-exported by the
  public `cypherlite` crate, and `surfaces/cypherlite-ffi` has no
  `Token`/AST references. No public API, C ABI, or protocol message
  changes; the C ABI is owned by a parallel effort (typed-abi-v2) and this
  work does not touch `surfaces/cypherlite-ffi`.
- **Contract.** openCypher TCK 3892/3892 keeps passing; lexer error text
  and TCK error-detail names (`Token::detail`, e.g.
  `InvalidUnicodeLiteral`) do not change.
- **Toolchain and deps.** MSRV Rust 1.88, edition 2021. No new heavy
  external dependency in a `cypherlite/*` crate (R6). `rustc-hash = "2"` is
  already a workspace dependency, used by `cypherlite-executor` for its
  core-store and fold maps.

Out of scope here: the lexer's byte-at-a-time scan (ASCII/SWAR fast path),
AST re-walk fusion in lowering, and planner-side maps — separate changes,
none of which needs a representation decision.

## Decision

Four parts, one goal — no heap allocation on the parse path that is not a
value the AST keeps.

### 1. Token literal: `Token<'a>` with `literal: Cow<'a, str>`

`Token` gains the input lifetime and its text becomes
`Cow<'a, str>`:

- **Borrowed from the input** for identifiers, keywords, numbers,
  parameters and string literals that contain no escape (the lexer scans
  the body; when it reaches the closing quote without a `\` it borrows
  the slice between the quotes).
- **Borrowed `&'static str`** for punctuation/operators (`"("`, `"<>"`, …)
  and fixed error texts — `'static` coerces to `'a`, no allocation.
- **Owned** only where the text is synthesized: escape-decoded string
  literals and `format!`-built error messages (the error path).

`tokenize`/`tokenize_with_options` return `Vec<Token<'a>>` tied to the
input; `Parser` becomes `Parser<'a>` holding `source: &'a str` (no source
copy) and `tokens: Vec<Token<'a>>`; `next_token` keeps a `clone()` that is
a pointer copy for the borrowed token (backup/restore re-reads consumed
tokens, so they stay in the vector). Every read site keeps reading `&str`
(`tok.literal.as_ref()` / deref), so the comment rule re-read and error
construction see exactly today's text; `ParseError` keeps owning its
message (one `to_string` on the error path only). The `detail` field is
unchanged.

### 2. AST strings: keep owned `String`s; operators become `&'static str`

The AST keeps owned `String` for names, labels, property keys and literal
values — the values it actually keeps. The gratuitous allocations go:

- Operator fields (`UnaryExpr::op`, `BinaryExpr::op` and the other
  `op: String` fields in `ast.rs`) become `&'static str`, drawn from the
  parser's existing operator constants (`OR`, …) — one table, consulted at
  every site, never re-spelled.
- An AST string is built from its token by one conversion
  (`Cow::into_owned` — a move when the token already owns its text,
  exactly one copy when it borrows); no intermediate `clone()`,
  `to_string()` of a `String`, or `format!` round-trip.

The AST stays `'static`, so `cypherlite-ir`, the planner, the executor
and the ontology crate see no lifetime change.

### 3. Keyword lookup: stack-buffer uppercase + byte-literal match cascade

`lookup_keyword` keeps its single match cascade as the one keyword table
(spellings, aliases `ASC`/`ASCENDING`, `INF`/`INFINITY`, all unchanged),
but matches on bytes without a heap copy:

- If `ident.len()` exceeds the longest keyword (10 bytes: `CONSTRAINT`,
  `DESCENDING`), return `TokenType::Identifier` immediately.
- Otherwise ASCII-uppercase the bytes into a fixed `[u8; N]` stack buffer
  and `match &buf[..len]` against byte-string literals (`b"MATCH"`, …).

Non-ASCII bytes pass through untouched, so a Unicode identifier
(ADR-0182) never matches a keyword — today's behavior. The compiler lowers
the byte-slice match to a length-then-compare decision tree, so the
cascade stays the fastest form once the allocation is gone. The signature
`lookup_keyword(&str) -> TokenType` is unchanged (its external callers —
`identifier_needs_quotes`, `tests/keyword_label_names.rs` — are
untouched).

### 4. Scope-map hasher: one process-wide seeded `RandomState` in `cypherlite-front`

Every per-statement identifier map and set in `cypherlite-front`
(`pattern_vars`, `output_vars`, `call_scope`, `lower/search_filter`, the
local-callable registry, the `validate/*` scope sets) is keyed by text the
query author wrote, so all of them hash from **one** source:
`cypherlite-front/src/ident_hash.rs` — a `static OnceLock<RandomState>`
drawn once per process from the OS RNG, exposed as the zero-sized
`IdentBuildHasher` and the `IdentHashMap`/`IdentHashSet` aliases. This is
the LABS-822 shape (`cypherlite-executor`'s used-sets hash from one
process-wide `RandomState`). No front map keys on an engine-controlled
value, so no front map keeps a fixed hasher, and the crate carries no
`rustc-hash` dependency.

Why pre-collision is impossible: the hasher is std's keyed SipHash-1-3,
a PRF under its 128-bit key; the key is process-random and never derived
from anything a query supplies, and no hash value is ever reflected into
a result or error text. Iteration order is the one channel through which
the key could become observable; the selection rule below closes it for
every user-visible choice. An attacker therefore cannot choose colliding identifiers
offline, and — unlike an unkeyed or merely seeded multiply-rotate hash —
there are no key-independent collisions to fall back on. The one shared
seed (rather than `RandomState::new()` per table) keeps table construction
free of the per-map thread-local seed read, and makes `IdentBuildHasher`
`Default`, so `IdentHashSet::default()` and
`with_capacity_and_hasher(n, IdentBuildHasher)` keep the pre-sizing the
FxHash form introduced (maps whose size is known at construction stay
pre-sized). Ordered or deterministic-iteration users (`BTreeMap`) stay as
they are.

**Selection rule.** Hash iteration order is process-random, so any
user-visible choice made from a hashed collection — the name an error
reports, an item that reaches output — selects deterministically:
`.min()` or sorted-first, never first-found. Enforcement sites: the
dynamic-label self-reference offender
(`validate/writes.rs::check_dynamic_label_scope`), the CALL-subquery
`VariableAlreadyBound` collision (`validate/scope.rs`, imported ∩ exported
∩ bound), and the GROUP BY alias-reference error
(`validate/aggregation/mod.rs`, sorted key variables); the existing
`.min()`/sorted-first sites (`call_scope.rs`, `writes.rs`'s mentioned-set
pick, `ast/collect.rs`'s undefined-name report) already followed it. Unit
tests with several simultaneous offenders pin the first two.

## Alternatives considered

### Token literal

- **`literal: &'a str` borrowed from the input only.** Rejected — the
  decoded text of an escaped string literal (`'a\nb'`, `'é'`,
  surrogate pairs) and the formatted error messages exist nowhere in the
  input, so a plain borrow cannot represent them without either a second
  owned side field (two text sources per token — parallel state) or
  moving escape decoding into the parser (changing where
  `InvalidUnicodeLiteral` is raised and risking its error text and
  position).
- **Span (start, end byte offsets) + slice accessor.** Rejected — same
  synthesized-text gap as the borrow, plus every reader (the comment-rule
  re-read, error construction, each of ~200 `.literal` sites in the
  parser) would need the source threaded to it to resolve text; the only
  saving over `Cow` is a few bytes per token, which the profile does not
  show as a cost.
- **Interned id table.** Rejected — interning hashes and stores each
  distinct token text, an allocation and a hash per new identifier,
  while the input string already outlives every token, so a borrow is
  strictly cheaper; the extra indirection would also touch every read
  site for no gain within a single parse.
- **Status quo (`String`).** Rejected — it is the measured top cost
  (1.9 allocations per token).

### AST strings

- **`Cow<'input, str>` in the AST.** Rejected — the AST outlives the input:
  `cypherlite-ir` embeds parser clauses (`Clause::Catalog`), the planner
  matches them, and graph-type bodies are kept by the executor and
  ontology crates. A borrowed AST drags the input lifetime through IR, the
  planner and every `Query` holder (or forces an `into_owned` deep copy at
  the boundary, giving the allocations back). The large share of the
  parser's allocations that are *not* kept values (operators, cloned
  tokens, re-copies) is removed by parts 1–2 without any lifetime.
- **Bumpalo-style arena of strings.** Rejected — a new external
  dependency in a `cypherlite/*` crate for a gain the borrowed tokens
  already capture, plus the same lifetime propagation as `Cow` (the arena
  must outlive every AST holder, including IR), and arena-allocated
  escaped literals still need the owned-copy path.
- **Operator enum instead of `&'static str`.** Deferred, not adopted
  here — the typed form is the better end state (no string discrimination
  of operators), but it rewrites every operator match in `cypherlite-front`
  and the IR lowering; `&'static str` removes the allocation with a
  mechanical change and leaves the enum as its own later decision.

### Keyword lookup

- **Case-insensitive comparison against a static slice of keywords**
  (`KEYWORDS.iter().find(|k| ident.eq_ignore_ascii_case(k))`). Rejected —
  a linear scan of ~75 entries per identifier is slower than the
  decision tree a `match` compiles to, and it splits the table into a
  slice plus a parallel `TokenType` column.
- **Hand-written length + first-byte switch, then exact compare.**
  Rejected — reproduces by hand what the compiler already emits for a
  byte-slice match, and duplicates the keyword vocabulary across switch
  arms (parallel state, drift risk).
- **Perfect hash (phf / gperf-style, build-time generated).** Rejected —
  a new dependency or build script for ~75 short keywords where the
  length-dispatched match is already near-optimal; once the allocation is
  gone `lookup_keyword` is no longer a measured cost.

### Scope-map hasher

- **Unseeded FxHash (`rustc-hash` `FxHashMap`/`FxHashSet`).** This PR's
  first form; rejected after the PR #721 security review — FxHash is
  deterministic and public, so one crafted statement can pre-collide its
  identifiers and force quadratic probing. The 4 MiB query-size cap
  (ADR-0257) bounds the input bytes, not that CPU work, and the
  per-statement map lifetime does not help when the statement itself is
  the attack.
- **Seeded FxHash (`rustc_hash::FxSeededState` with one process-random
  seed).** Rejected — it keeps Fx speed but not precomputation
  resistance: `FxHasher::write` compresses a byte string through
  `hash_bytes` with fixed constants before the seeded state is touched,
  so identifiers that collide in that compression collide under every
  seed.
- **ahash or another DoS-resistant fast hasher.** Rejected — a new
  external dependency (R6 review) where std's keyed SipHash already
  answers the attack and the executor's LABS-822 precedent uses it.
- **Std default `RandomState::new()` per table (the pre-ADR state).**
  Rejected — DoS-resistant, but each table construction pays the
  thread-local seed read; the one shared seed keeps the resistance
  without that per-map cost.
- **Sorted `Vec`s for small scopes.** Not adopted as the policy — a
  per-site optimization a later change may apply where a scope is
  provably tiny; the hasher switch is the uniform fix.

## Consequences

- `Token`, `Parser` and the `tokenize*` functions gain a lifetime
  parameter; `pub use token::{Token, TokenType}` keeps exporting the
  (now generic) type. No crate outside `cypherlite-parser` names `Token`,
  so the change stays inside the parser crate, plus the AST operator
  field type in `ast.rs` and the operator comparisons in
  `cypherlite-front`/`cypherlite-ir` that read it (`&'static str`
  compares with `==` like `String` did).
- Expected effect: most of the 87 lexer allocations per statement and
  the parser's token-clone and operator allocations disappear; the
  remaining parser allocations are the AST's kept values and its boxes.
  The implementing changes report before/after allocations per statement
  and µs/statement on the same corpus harness.
- Error text, error positions and TCK detail names are unchanged by
  construction (same strings, read through `&str`); the TCK 3892/3892
  floor and the lexer/parser unit suites gate it. Escaped string literals
  keep lexer-time decoding and lexer-time `InvalidUnicodeLiteral`.
- `cypherlite-front` adds no dependency: the hasher is std's. The
  collision-attack exposure of the scope maps is resolved, not bounded —
  the keyed, process-random SipHash leaves a crafted statement no offline
  way to pick colliding identifiers (§4). The cost is SipHash's per-key
  work over Fx's: on the front_bench corpus (2103 statements) lowering
  measured ≈16.1–16.7 ms/pass with FxHash and ≈17.0–17.5 ms/pass with the
  shared-seed SipHash, still well under the 26.35 ms/pass pre-ADR
  baseline. Allocations per statement are unchanged (156.0 on the current
  corpus either way; the hasher allocates nothing and the shared seed
  never touches the heap).
- Iteration order of the scope maps varies across processes (as it did
  under the pre-ADR per-table SipHash; the FxHash form was deterministic).
  The TCK floor pins error texts only within one process, so it cannot
  catch a hash-order pick; the §4 selection rule is what pins
  user-visible names across processes, and a new selection from a hashed
  collection must follow it.
- No public API, C ABI, CLI, Bolt/HTTP/MCP protocol, or documentation
  surface changes; no `docs/api/` update is required.
