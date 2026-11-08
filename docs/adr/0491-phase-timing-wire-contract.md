# ADR-0491: Per-surface phase-timing wire contract (parse / plan / execute)

Status: Proposed

Claim: e54e204222fee8508b47a0469dd69f4039d4d2d0

## Context

LABS-832 (neo4j-timing-alignment) aligns CypherLite's server-side timing
reporting with what Neo4j-family tools display. Today one number exists:
the ADR-0221 engine-reported `elapsed` — a single wall-clock `Duration`
per statement, converted once in `QueryResult::elapsed_ms()`
(`cypherlite/cypherlite/src/result.rs`) and rendered by every surface.
On the wire, Bolt carries only the streaming instants: `t_first` on RUN
SUCCESS and `t_last` (integer milliseconds) on the ending PULL/DISCARD
SUCCESS, per ADR-0309. No surface can answer the first question a slow
statement raises — is the time in parsing, in planning, or in execution?

The engine-phase-capture node (commit 1744bc81f, landed on the feature
branch) changed that: `cypherlite::phase_timing::PhaseTimings`
(`cypherlite/cypherlite/src/phase_timing.rs`) now measures the PARSE /
PLAN / EXECUTE split always-on — no PROFILE opt-in, no feature flag.
The three phases partition the same measured body the ADR-0221 total
spans (PARSE: front-end parse and lowering; PLAN: binding, logical
planning, admission classification; EXECUTE: the dispatch, or the
EXPLAIN plan render), so they are disjoint sub-windows whose sum never
exceeds `elapsed`. The type is `pub(crate)`: its exposure — which keys,
which types, which placement, on which surfaces — was left to this ADR.

The research node's evidence (recorded in the feature graph) settles the
external contract: an established line of Bolt server tooling sends
`parsing_time`, `planning_time` and `plan_execution_time` as PackStream
Float **seconds** in the ending PULL/DISCARD SUCCESS, and the console
clients of that ecosystem display exactly these three keys — so tooling
exists that renders these names and nothing else. All four official
Neo4j drivers ignore unknown
SUCCESS keys (the Go hydrator's default arm drops them, Java and JS
read known keys only) except the Python driver, which exposes the raw
dict as `ResultSummary.metadata` — extra keys display free there and
break nothing.

## Decision

The phase split crosses every surface as a per-surface rendering of one
engine fact — the three `Duration`s of `PhaseTimings` — following the
established pattern where the rendering is the surface's and the fact
is the engine's, like `stats` (Bolt kebab-case keys vs envelope
snake_case) and timing units (`t_last` integer ms on Bolt vs
`elapsed_ms` f64 ms everywhere else).

1. **Bolt**: send `parsing_time`, `planning_time` and
   `plan_execution_time` as PackStream Float **seconds** in the ending
   PULL/DISCARD SUCCESS, beside `t_last` — not in RUN SUCCESS, whose
   contract (ADR-0309) stays unchanged. The names, types and placement
   match the keys established Bolt console tooling renders, chosen because
   keys no tool displays are dead weight on a wire: the probed consoles
   display these three as-is. The values must never be mixed with
   integer-millisecond
   numbers under these names — tools parse them as doubles. `t_first`
   and `t_last` semantics and placement are untouched.

2. **Own surfaces**: extend the existing `elapsed_ms` family.
   - Rust API: `parse_ms()`, `plan_ms()`, `execute_ms()` (f64) beside
     `elapsed_ms()` on `QueryResult` (`cypherlite/cypherlite/src/result.rs`).
   - The canonical wire envelope — the FFI handle API's response, WASM's
     `execute`/`executeScript`, and every Rust `execute_wire`/
     `run_script_wire` consumer: `parse_ms`/`plan_ms`/`execute_ms` beside
     `elapsed_ms`, emitted from the one canonical `wire::response_envelope`
     (`cypherlite/cypherlite-model/src/wire.rs`). (The CLI's own machine
     modes render a different, human-tooling shape that has never carried
     `elapsed_ms`; its timing rendering is the human `Time:` line below.)
   - The HTTP Query API v2 reply carries none of them, `elapsed_ms`
     included: its success envelope mirrors the Neo4j reference server's
     shape exactly (a recorded compatibility decision — "extension keys
     are deliberately not invented here", `docs/api/http/spec.md`), and
     diverging it to carry timing would trade a working Browser
     compatibility guarantee for information every other surface already
     delivers. Its spec names where the fields live, exactly as it
     already does for `elapsed_ms`.
   - CLI human output: extend the existing `Time:` line
     (`surfaces/cypherlite-cli/src/exec_query.rs`) to
     `Time: 12.3ms (parse 0.4 / plan 1.1 / execute 10.8)`.
   - C ABI (FFI): `cypherlite_result_parse_ms`,
     `cypherlite_result_plan_ms`, `cypherlite_result_execute_ms` and
     the matching result-struct fields, mirroring
     `cypherlite_result_elapsed_ms`
     (`surfaces/cypherlite-ffi/src/tabular.rs`).
   - Python: `parse_ms`, `plan_ms`, `execute_ms` attributes beside
     `elapsed_ms` (`surfaces/cypherlite-python/src/result.rs`).

3. **Units**: Bolt renders seconds (f64) because that is the unit the
   probed console tooling parses; every other surface renders milliseconds (f64)
   because that is the `elapsed_ms` family it extends. Both are
   conversions of the same engine `Duration`s — the unit convention
   stays at the existing conversion points, nowhere new.

4. **The total stays the ADR-0221 `elapsed`**: `elapsed_ms` keeps its
   meaning and its conversion; the three fields are additive context,
   never a replacement. Their sum may be less than `elapsed` (the
   inter-stage gaps — post-EXPLAIN gates, token reservation — stay in
   the total alone), never more.

5. **Always-on**: the measurement cost is one `Instant` read per phase
   boundary per statement, already paid since the capture node landed —
   no opt-in, no feature flag, no preview gate. This ADR decides only
   rendering and placement, not measurement.

## Alternatives considered

- **`t_parse` / `t_plan` / `t_execute`, integer milliseconds — the
  Neo4j-family naming.** Consistent with `t_first`/`t_last` (ADR-0309)
  and keeps one unit on the Bolt summary. Rejected: no tool displays
  these keys — they would be dead weight on the wire — and they squat
  Neo4j's reserved `t_*` summary namespace, so a future Bolt spec
  defining them differently collides with us. Recorded as the fallback
  if these keys ever prove costly.

- **PROFILE-only exposure.** Phase timings appear only when the
  statement is wrapped in `EXPLAIN`/`PROFILE`, like the plan argument
  today. Rejected: a plain query — the case where a user asks "why was
  this slow?" — would carry no split; the engine already measures
  always-on, so gating the wire field would reintroduce an opt-in the
  capture node deliberately removed; and PROFILE's own summary would
  then double-report execution cost under two names.

- **Per-surface ad-hoc names.** Each surface picks its own field names
  and units for the split. Rejected: violates the result-metadata
  alignment rule (one surface gains a piece → every surface that can
  expose it gets it, same change) and forks one engine fact into
  parallel spellings clients must reconcile — the exact drift ADR-0221
  removed for the total.

- **HTTP Query API extension keys.** Add `elapsed_ms` and the three
  phase fields to the HTTP v2 success reply as CypherLite extension
  keys. Rejected: that reply's recorded contract is byte-for-byte the
  Neo4j reference server's shape, and its Browser compatibility rests
  on matching it exactly — extension keys would spend that guarantee on
  information Bolt, MCP, the CLI and every embedded binding already
  deliver. The alignment rule binds surfaces that *can* expose a piece
  without breaking their own contract; the HTTP reply cannot.

## Consequences

- Every surface renders the same three fields from the one
  `PhaseTimings` — the cross-surface result-metadata alignment holds by
  construction, and the exposure nodes must land all surfaces in their
  change (docs, examples, client reference bundles, the Python library
  skill — the §2 documentation burden).
- Official drivers are unaffected: Go, Java and JS ignore unknown
  SUCCESS keys; the Python driver surfaces them for free through
  `ResultSummary.metadata`, so the split displays in tooling without
  any driver change.
- By not squatting the `t_*` namespace, a future Bolt specification
  that defines its own phase keys cannot collide with ours; the
  established-contract keys are the aligned surface instead.
- The split is additive context beside the ADR-0221 total: clients that
  only read `elapsed_ms` (or only `t_last`) see no change.
- The always-on cost — a handful of `Instant` reads per statement — is
  already carried by the landed capture node and covered by the
  baseline loadtest envelopes; no new knob, nothing to back out.
- Bolt consumers that parse the ending SUCCESS strictly (unknown-key
  rejection) would see three new keys; none are known in the probe set
  — the four official drivers and the probed console clients all
  tolerate them.
- The always-on split coexists with the pre-existing opt-in commit-timing
  clock (`timing_on` → `parse_plan_ns`, nanoseconds, the commit-timing
  policy). The two clocks serve distinct policies — result metadata in
  milliseconds vs commit diagnostics in nanoseconds — with deliberately
  different windows: the commit clock starts before the health/failure
  checks and survives failed admission, the phase clock spans only the
  measured statement body. Unifying them is a future decision, recorded
  here so the coexistence reads as decided rather than accidental.

## Later references

- ADR-0540 extends this split: the fine sub-split (lexing+AST / rewrite /
  validation+lowering / result generation), the plan-cache outcome, and —
  reversing this ADR's exclusion of them — timing keys on the CLI's
  machine modes and the HTTP Query API v2 envelope (namespaced after the
  Neo4j members). This ADR's Bolt contract keys and the canonical
  envelope's flat `_ms` family are unchanged; the new keys join them.
