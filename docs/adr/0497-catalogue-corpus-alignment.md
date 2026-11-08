# ADR-0497: Align the built-in function and procedure catalogues with the Cypher-25 corpus

Status: Proposed

Claim: dd5de9932d79f78115ef59cc06fab3419abf1471

## Context

CypherLite's built-in catalogues grew additively, one CIP and TCK wave at a
time, and the Cypher-25 harness corpus now pins the reference catalogue row by
row: a 195-row built-in `SHOW FUNCTIONS` table
(`features/correctness/ShowFunctions.feature:61-201`) and a 93-row
`SHOW PROCEDURES EXECUTABLE` table
(`features/correctness/ShowProceduresPart2.feature:73-167`), asserting each
row's name, category, mode, `worksOnSystem` flag, description and CIP-113
signature.

The engine's inventories have diverged from it in three directions.
`KNOWN_FUNCTIONS` (`cypherlite/cypherlite-front/src/validate/functions.rs`,
206 entries) plus the four catalog-only `graph.*` rows
(`cypherlite/cypherlite-executor/src/function_registry.rs:480`, rendering
through `cypherlite/cypherlite-executor/src/show_catalog.rs:110`) produce 212
`SHOW FUNCTIONS` rows today against the corpus's 195.
`BUILTIN_PROCEDURE_SIGNATURES`
(`cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:101`)
carries 42 rows, of which two (`db.indexes`, `db.constraints`) occur nowhere in
the corpus, while 54 entries the corpus inventories are missing entirely --
including `db.stats.retrieve`, which the corpus calls seven times
(`features/correctness/GraphCounts.feature:38` and siblings) and which fails
today with `ProcedureNotFound` in all six executed scenarios. And the metadata
the corpus asserts per row — description, category, mode, `worksOnSystem`,
`listed` — is missing or drifted on dozens of rows whose names already match.

The corpus execution stands at `ShowProcedures` 10 pass / 18 fail of 28,
`ShowProceduresPart2` 3 pass / 2 fail / 7 skipped of 12, `ShowFunctions` 9 pass
/ 8 fail / 15 skipped of 32, `ShowFunctionsPart2` 3 pass / 17 fail / 13 skipped
of 33 and `ShowFunctionsPart3` 1 pass / 1 fail / 6 skipped of 8. Name-level
alignment does not close every one of those failures — parsing
`SHOW PROCEDURES EXECUTABLE` / `EXECUTABLE BY` is out of scope here (§10) --
but it is a precondition for all of them.

Three authorities speak to the catalogue and they disagree in five places: the
corpus (generated catalogue tables, cross-checked by live probes against Neo4j
2026.05/2026.09.0), the CIP documents (accepted proposals, sometimes ahead of
any release) and the LABS-874 issue list (an unchecked shorthand whose five
disputed entries are exactly the five conflicts in §5). Without one written
rule, every CIP wave re-litigates the same calls, the preview registry
(ADR-0371) has no policy for names, and an engine keeps advertising functions
no reference server has — or deletes surface an approved CIP pins.

## Decision

### 1. Source of truth: the corpus is first-tier

The Cypher-25 harness corpus, executed and live-verified, is first-tier for
names, spellings, modes, `worksOnSystem` flags, descriptions and signatures: a
corpus row wins. CIP documents are second-tier and are consulted only where the
corpus is silent. LABS-874's list is third-tier and loses wherever either of
the other two contradicts it (`typename`, `distance`, `apoc.coll.sort`).

Where the corpus contradicts itself, the engine-observable probe wins over the
generated catalogue table: a row a reference server lists in a table but
answers 42N48 for, with no `SHOW` row on the same version, is not surface
(§5.2). Both readings are reproduced with their citations in §5, so the
tie-break is checkable rather than asserted.

The review consequence: a catalogue divergence needs a recorded reason naming a
corpus row, a CIP, or this ADR. Growing the inventory because some document
lists a name is not one.

### 2. The release-status split, in four rules

- **(a) Default surface.** A name a released reference server registers is
  implemented, advertised and callable in every build.
- **(b) Preview-gated.** A name an approved CIP adds that no release ships, or
  that the corpus records as unreachable or `@doc`-derived against the live
  server, goes behind ADR-0371's registry (§3) and its cip-eval directory joins
  `GATED_CIPS`. Never deleted, never silently advertised.
- **(c) Removed.** A name the corpus records as removed leaves both the
  callable and the advertised registry — an unknown function in every dialect
  — and any lowering-internal use moves to `UNADVERTISED_FUNCTIONS`.
- **(d) Added.** A name the corpus has and the engine lacks joins the registry
  and its dispatch/catalogue in the same change.

A rename is (c) plus the target's existing row: the old spelling goes, the
canonical spelling already exists, and no new dispatch arm is written. All ten
rename targets — `ltrim`, `rtrim`, `relationships`, `valuetype`,
`vector_norm`, `vector_dimension_count`, `vector.similarity.cosine`,
`vector_distance`, `stdev_samp`, `stdev_pop` — are existing registry rows
today, so every rename in §8 is a deletion plus a metadata merge, never new
surface.

### 3. The three preview rows

| Feature | Variant | Names | CIP | Gated surface |
| --- | --- | --- | --- | --- |
| `coll-zip` | `CollectionZip` | `coll.zip` | CIP-274 (approved 2026-09-03, unreleased) | callable and `SHOW`-listed |
| `coll-has` | `CollectionHas` | `coll.hasAnyOf`, `coll.hasAllOf`, `coll.hasNoneOf` | CIP-286 (approved 2026-10-15, unreleased) | callable and `SHOW`-listed |
| `gql-stddev-aliases` | `GqlStddevAliases` | `stddev_pop`, `stddev_samp` | CIP-245 (stddev half: aggregation [19]-[26], show_functions [7]-[8]) | callable and `SHOW`-listed |

Each of the three gates a catalogue row: hidden from `SHOW FUNCTIONS` while
off, listed while on. (A fourth row, `apoc-coll-sort`, was deleted outright on
2026-11-04 — see §5.3.)

Each row is one `preview_feature_registry!` entry in
`cypherlite/cypherlite-core/src/preview.rs` (variant + doc literal + kebab
name, appended, never renumbered) and one `GATED_CIPS` row in
`testing/cypherlite-cip-eval/tests/cip_eval.rs:18`, whose one table derives
both the phase's directory filter and its `CYPHERLITE_PREVIEW` value:

| CIP | testdata directory | Flag |
| --- | --- | --- |
| CIP-274 | `CIP-274-collection-functions-coll-zip` | `coll-zip` |
| CIP-286 | `CIP-286-collection-functions-coll-has` | `coll-has` |
| CIP-245 | `CIP-245-gql-function-aliases` | `gql-stddev-aliases` |

No baseline drops: `baselines/cip-eval.json` keeps CIP-245 at 78/78, CIP-150 at
28/28, CIP-274 at 13/13 and CIP-286 at 44/44, and every frozen CIP-245 (76)
identity keeps passing in the gated phase. Each row is temporary by
construction; it and its gate are deleted when the CIP ships in a reference
release (CIP-274, CIP-286, CIP-245). CIP-150 left `GATED_CIPS` on 2026-11-04
with the `apoc-coll-sort` deletion (§5.3): its scenarios now run in the
ungated phase, where scenario [5] was re-pinned to 42N48 and
`ENGINE_PASSING.json` gained that identity.

### 4. Gate locality: the function-name admission gate

ADR-0371's scope policy admits only **syntax-extending** features; a purely
additive function name never enters the registry. Four gate-location
exceptions sit off that parse/dispatch point: `obfuscated-literals` reads its
flag in the lexer (ADR-0454), `complex-search-patterns` in front validation
through `ValidateOptions` (ADR-0467), the `db.ontology.*` family at procedure
dispatch because its contract tracks a non-final external spec (ADR-0378), and
a gated function name in the pre-binder admission walk below — this ADR's
addition and the narrowest of them. **A name gate hides a name; it never
widens grammar.** The exception is recorded where the policy lives, in
ADR-0371's Later references, in the same commit as this ADR.

The gate is an *admission* gate, checked where the name is admitted:
`validate_function_admission` walks the parsed statement tree before lowering
and rejects a call to a gated name with the ordinary `UnknownFunction` (42N48)
unless the store's preview set enables its feature. It has exactly five touch
points:

1. **The registry row** — one `preview_feature_registry!` entry per feature in
   `cypherlite/cypherlite-core/src/preview.rs:156-180` (variant, doc literal,
   kebab name), appended, never renumbered (§3).
2. **The name table** — `PREVIEW_FUNCTIONS`
   (`cypherlite/cypherlite-front/src/validate/functions.rs:283`): one row per
   gated name carrying its feature; a gated name is catalogue surface exactly
   while that feature is enabled.
3. **Admission** — `validate_function_admission`
   (`validate/functions.rs:352`), the pre-binder gate: a whole-tree
   `walk_statement_tree` pass over every `Expr::FunctionCall`, wired into the
   lowering's validation battery
   (`cypherlite/cypherlite-front/src/lower/mod.rs:86`) after the dialect gates.
   A rejected name never reaches the binder.
4. **Listing** — `function_catalog_rows(preview)`
   (`cypherlite/cypherlite-executor/src/show_catalog.rs:104`) filters through
   `function_passes_name_gate` (`validate/functions.rs:332`), exactly as
   `procedure_catalog_rows(preview)` (`show_catalog.rs:188`) already does for
   ADR-0378's ontology family.
5. **The front gate loop** — one `Ok(false)` arm in
   `parse_and_lower_statement_with_policy_and_params`'s exhaustive
   `PREVIEW_FEATURES` match (`cypherlite/cypherlite-front/src/lib.rs:218-220`),
   because the gate is name admission, not an AST detector: the arm records
   that the feature has no post-parse construct, and a registry row without one
   is a compile error.

`AGGREGATE_ALIASES` (`cypherlite/cypherlite-front/src/agg.rs:21`) is unchanged:
the `stddev_samp` / `stddev_pop` entries stay, because the alias table needs no
gate of its own. It is consulted during lowering (`lower_function_name`,
`cypherlite/cypherlite-front/src/lower/expr.rs:81`) to give a spelling its
`stDev`/`stDevP` display name, and the admission walk runs before lowering — so
a disabled alias is rejected on its parsed spelling and never reaches the
canonical name it would otherwise bind through.

One owner for the mapping: `function_preview_feature(name)` beside
`KNOWN_FUNCTIONS`, the function-side twin of
`procedure_gate::procedure_preview_feature`: admission and listing read the
same `PREVIEW_FUNCTIONS` row, so a name cannot be rejected while advertised or
admitted while unlisted. `UNADVERTISED_FUNCTIONS` stays what it is — the
lowering's internal spellings, consulted by the binder's `is_known_function`
(`validate/functions.rs:382-389`) alone — so moving
`trim_leading`/`trim_trailing` there drops their `SHOW` rows without rejecting
the parser's own lowered form.

**Deliberate deviation from ADR-0371's authored-error rule.** A gated name's
disabled form is the registry's ordinary `UnknownFunction` rejection (42N48),
not `preview feature '<name>' is not enabled: set CYPHERLITE_PREVIEW=<name>`.
The authored error exists so a user who wrote new *syntax* learns the flag; a
name that must not exist cannot be named in an error without advertising it,
and 42N48 is what the reference server answers for an unregistered name. The
`preview.rs` module documentation's "one exception, not general" paragraph is
rewritten to name the procedure-family and name-admission exceptions, so the
policy text matches the registry.

### 5. The five conflicts, settled

#### 5.1 `stddev_samp` / `stddev_pop` — gate

Final action: the default dialect advertises and dispatches `stdev_samp` /
`stdev_pop`; the CIP-245 spellings stay callable and `SHOW`-listed only under
`CYPHERLITE_PREVIEW=gql-stddev-aliases`, and CIP-245's directory joins
`GATED_CIPS`.

Reason: the corpus is first-tier and lists only the `stdev_*` rows
(`features/correctness/ShowFunctions.feature:207-208`). CIP-245's stddev
scenarios are `@doc`-derived — Neo4j 2026.05 answers 42N48, and their expected
values were computed from `stDevP`/`stDev`
(`testdata/cip-eval/CIP-245-gql-function-aliases/NOTES.md` Deviation 1) --
while `baselines/cip-eval.json` holds CIP-245 at 78/78. Re-pinning CIP-245 is
out of scope and the ratchet only moves up (ADR-0001), so the gate keeps both:
a corpus-faithful default dialect and CIP-245's 78/78, with its 76 frozen
identities passing in the gated phase.

Rejected: keep `stddev_*` advertised — contradicts the corpus catalogue, and
the two extra rows break the whole-catalogue and ordering scenarios. Keep them
only as `UNADVERTISED_FUNCTIONS` — CIP-245 `show_functions` [7]/[8] then fail
(76/78) because they assert the `SHOW` rows. Re-pin CIP-245 (drop aggregation
[19]-[26] and `show_functions` [7]-[8], lower `baselines/cip-eval.json`) — out
of scope, and it lowers a frozen ratchet.

#### 5.2 `coll.zip` — gate

Final action: callable and `SHOW`-listed only under
`CYPHERLITE_PREVIEW=coll-zip`; CIP-274's directory joins `GATED_CIPS`. In the
default dialect it is an unknown function (42N48) with no `SHOW FUNCTIONS` row.

Reason: the corpus contradicts itself here, and the engine-observable probe
wins over the catalogue table.
The corpus's function-expressions acceptance rows record a live
probe — `coll.zip` registered in the front end but unreachable:
`SHOW FUNCTIONS` lists no row and both CYPHER 5 and CYPHER 25 raise 42N48 --
while the generated table at `ShowFunctions.feature:90` still carries the row.
CIP-274 was approved on 2026-09-03 and no release ships it, so advertising the
name would claim surface the reference server does not have (rule (b) against
rule (a)).

Rejected: advertise and call it unconditionally, following
`ShowFunctions.feature:90` — contradicts the only live probe and ADR-0371's
accepted-but-unreleased split. Delete the implementation and CIP-274's testdata
-- throws away a passing 13/13 ratchet for a name that is already implemented.

#### 5.3 `apoc.coll.sort` — remove (amended 2026-11-04)

Amended final action (2026-11-04, LABS-912): delete the spelling outright — the
registry row, the executor dispatch alias, and the `apoc-coll-sort` preview
name gate, together with the `validate_no_apoc_coll_sort` dialect gate. A call
is now the binder's ordinary unknown-function rejection (42N48) in every
dialect, `CYPHER 5` included. CIP-150 leaves `GATED_CIPS` and runs in the
ungated phase; its scenario [5] is re-pinned to the same 42N48 that scenario
[6] asserts (a Cypher-5 expectation that conflicts with Cypher 25 goes to the
latter), so it turns green and its identity joins `ENGINE_PASSING.json`. No
baseline count drops: CIP-150 stays 28/28.

Reason: the original decision below gated the name behind
`CYPHERLITE_PREVIEW=apoc-coll-sort` and admitted it only under `CYPHER 5`,
keeping the dialect gate for Cypher 25. Retaining an APOC library name in the
Cypher surface at all — even behind a preview opt-in and a dialect — is the
wrong shape once CIP-222's `coll.sort` covers the need: the name is absent
from the corpus catalogue and is not a Cypher built-in. The one scenario that
justified the gate, CIP-150 [5], is a `@doc`-derived transcription its own
notes say could not be verified, because APOC is not installed on the eval
server
(`testdata/cip-eval/CIP-150-cypher-versions-selection/NOTES.md`); re-pinning
it to the observed error keeps the ratchet intact. The spelling can return
later as an extension if needed.

Superseded action (original decision, 2026-11-04): callable only under
`CYPHERLITE_PREVIEW=apoc-coll-sort`, and only in the CYPHER 5 dialect;
CIP-150's directory joined `GATED_CIPS`. Without the flag it was an unknown
function in every dialect; under it, CYPHER 5 accepted it (CIP-150 [5]) while
CYPHER 25 rejected it through the unchanged dialect gate
(`validate_no_apoc_coll_sort`). It was never `SHOW`-listed in either dialect,
so the flag changed callability only.

Rejected (amended): keep the gate — retains an APOC name in the Cypher 5
surface and a second, dialect-specific rejection path for a name no reference
release ships, where the ordinary unknown-function rejection already covers
both dialects. Rejected (original): keep it callable in CYPHER 5
unconditionally — fails rule (c) (a removed name errors in every dialect) and
keeps an APOC name in the Cypher 5 surface. Rejected (original): delete it and
drop scenario [5] — lowers the ratchet; re-pinning [5] to the observed error
avoids that.

#### 5.4 `typename` — rename to `valueType`

Final action: rename `typename` to `valueType`.

Reason: LABS-874's `typename -> type` is a collision, not a rename. `type` is
the RELATIONSHIP-type function (`ShowFunctions.feature:246`) with different
behaviour, so the rename would silently change what every existing `type()`
call returns. CypherLite's `typename` computes the value-type name
(`cypherlite/cypherlite-executor/src/eval/function/scalar.rs:38-41`), and the
corpus's row for exactly that job is `valueType` (`ShowFunctions.feature:253`,
exercised by `features/acceptance/TypePredicateExpressionAcceptance.feature:34`),
which already carries the same implementation.

Rejected: LABS-874's literal `type` target — collides with the relationship-type
function and changes `type()` semantics. Keep `typename` as an alias — the
corpus lists no `typename` row, and an extra advertised row breaks the
catalogue scenarios.

#### 5.5 `distance` — remove

Final action: remove `distance`; the canonical spelling of the same computation
is `point.distance`.

Reason: LABS-874's `distance -> possibly vector_distance` is a false alias.
CypherLite's `distance()` is point distance
(`cypherlite/cypherlite-executor/src/eval/function/spatial.rs:25` ->
`point_distance`), while `vector_distance` is a different vector function
taking a metric argument; the corpus lists `point.distance`
(`ShowFunctions.feature:176`) and `vector_distance` (`:258`) as separate rows,
and pins `distance()` as a removed function returning 42N48
(`features/correctness/SemanticErrorPart3.feature:831-835`).

Rejected: LABS-874's `vector_distance` target — would make a point-distance
call report a vector metric error, and duplicates an existing function. Keep
`distance` as an alias of `point.distance` — the corpus explicitly pins its
rejection (42N48), so an alias would be a documented divergence.

### 6. The procedure inventory

- **`db.indexes` and `db.constraints` are deleted** from
  `BUILTIN_PROCEDURE_SIGNATURES` (`signatures.rs:119,121`). Both appear in no
  corpus file, the Cypher-25 procedure inventory has no row for either, and the
  corpus reaches those surfaces through `SHOW INDEXES` / `SHOW CONSTRAINTS`
  (ADR-0116's command set). After removal `CALL db.indexes()` and
  `CALL db.constraints()` fail as unknown procedures and `SHOW PROCEDURES`
  stops listing them; no replacement procedure is added, because the `SHOW`
  commands are the surface.
- **`db.stats.retrieve` is added as a real executable READ procedure**:
  `db.stats.retrieve(section :: STRING) :: (section :: STRING, data :: MAP)`,
  implementing the GRAPH COUNTS section from the engine's live
  constraints/indexes/graph types (the shape `features/correctness/GraphCounts.feature`
  pins). It is not a catalog-only row: the corpus calls it, so a listing-only
  stub would trade `ProcedureNotFound` for a wrong answer.
- **53 listing-only rows are added** (`ProcSignature::read(...).executable(false)`,
  listed): inventory entries the corpus lists, no corpus scenario calls, and the
  engine has no such operation to run — so `SHOW PROCEDURES` matches without
  inventing execution.
- **Six callable-but-unlisted driver rows become listed**: `tx.getMetaData`,
  `tx.setMetaData`, `dbms.components`, `dbms.info`, `dbms.showCurrentUser`,
  `dbms.listConfig`.
- **Metadata is filled from the corpus**: description, category, CIP-113
  signature, mode, `worksOnSystem` and `listed` follow the corpus row by row.
  The corpus pins description text verbatim — `db.awaitIndex` expects
  `Wait for an index to come online (for example: CALL db.awaitIndex("MyIndex", 300)).`
  and signature `db.awaitIndex(indexName :: STRING, timeOutSeconds = 300 :: INTEGER)`
  (`features/correctness/ShowProcedures.feature:213,224`) — so aligning names
  alone leaves the description, signature and mode scenarios failing.
- **The 42 corpus test-fixture names stay absent** (`newProc`, `oldProc`,
  `apoc.meta.*`, the `org.neo4j.*` family, and the rest): CypherLite exposes no
  procedure-registration surface (`steps/cypher_steps.py:60-66` skips the
  registration step), so absence is the aligned state — no row, no listing, no
  `CALL`. The 93-row `SHOW PROCEDURES EXECUTABLE` table's five `TestProcedures`
  rows can therefore never come from the engine.

### 7. What the alignment does not touch

- `db.ontology.*` stays behind `CYPHERLITE_PREVIEW=ontology` (ADR-0378),
  unchanged.
- `vector.dimensions` survives as the DDL `indexConfig` option key
  (`cypherlite/cypherlite-executor/src/vector_similarity.rs:77`,
  `cypherlite/cypherlite-ontology/src/validate/ext.rs:61`): the option is a
  configuration key, not a function name, and rule (c) retires the callable
  alias only.
- The four catalog-only `graph.*` rows are unchanged.
- Docs change in the cutover commits, not in this one: `docs/reference/functions.md`,
  `docs/guides/geospatial-queries.md`, `docs/guides/vector-similarity.md`,
  `docs/reference/cypher-syntax.md`, `docs/api/cli/spec.md`'s
  `CYPHERLITE_PREVIEW` row (twelve -> fifteen live features),
  `surfaces/cypherlite-cli/src/skill/bundle/references/vector.md` and
  `AGENTS.md`'s Cypher-dialect gated-feature list.

### 8. Function disposition (213 rows)

Every row of the alignment policy, verbatim. `Action` is one of **keep** (name
stays callable and `SHOW`-listed, unchanged spelling), **rename** (the old
spelling stops being callable and listed; the canonical target — an existing
row — carries the implementation), **gate** (callable and `SHOW`-listed only
while its flag is enabled), **remove** (the name stops being callable and
listed, the ordinary `UnknownFunction` rejection in every dialect) or **add**
(new lowercase registry row plus dispatch/catalogue). `Flag / canonical` names
the preview flag for a gate and the surviving spelling for a rename.
`db.indexes` and `db.constraints` carry a `remove` row here because the policy
audits them on both axes; the change itself is the registry-row deletion in §6.

| Name | Action | Flag / canonical | Detail |
| --- | --- | --- | --- |
| `id` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:133 (Cypher 25 catalogue row `id`) |
| `elementid` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:119 (Cypher 25 catalogue row `elementId`) |
| `db.namefromelementid` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:110 (Cypher 25 catalogue row `db.nameFromElementId`) |
| `normalize` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:166 (Cypher 25 catalogue row `normalize`) |
| `labels` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:137 (Cypher 25 catalogue row `labels`) |
| `type` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:246 (Cypher 25 catalogue row `type`) |
| `keys` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:136 (Cypher 25 catalogue row `keys`) |
| `properties` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:178 (Cypher 25 catalogue row `properties`) |
| `size` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:201 (Cypher 25 catalogue row `size`) |
| `isempty` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:134 (Cypher 25 catalogue row `isEmpty`) |
| `length` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:140 (Cypher 25 catalogue row `length`) |
| `head` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:132 (Cypher 25 catalogue row `head`) |
| `tail` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:215 (Cypher 25 catalogue row `tail`) |
| `last` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:138 (Cypher 25 catalogue row `last`) |
| `reverse` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:189 (Cypher 25 catalogue row `reverse`) |
| `range` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:183 (Cypher 25 catalogue row `range`) |
| `coalesce` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:80 (Cypher 25 catalogue row `coalesce`) |
| `tostring` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:240 (Cypher 25 catalogue row `toString`) |
| `tostringlist` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:241 (Cypher 25 catalogue row `toStringList`) |
| `uuid` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:248 (Cypher 25 catalogue row `uuid`) |
| `uuid.mostsignificantbits` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:252 (Cypher 25 catalogue row `uuid.mostSignificantBits`) |
| `uuid.leastsignificantbits` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:251 (Cypher 25 catalogue row `uuid.leastSignificantBits`) |
| `randomuuid` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:182 (Cypher 25 catalogue row `randomUUID`) |
| `tointeger` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:236 (Cypher 25 catalogue row `toInteger`) |
| `tofloat` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:233 (Cypher 25 catalogue row `toFloat`) |
| `toboolean` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:230 (Cypher 25 catalogue row `toBoolean`) |
| `tolower` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:239 (Cypher 25 catalogue row `toLower`) |
| `toupper` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:243 (Cypher 25 catalogue row `toUpper`) |
| `upper` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:247 (Cypher 25 catalogue row `upper`) |
| `lower` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:159 (Cypher 25 catalogue row `lower`) |
| `trim` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:244 (Cypher 25 catalogue row `trim`) |
| `trim_leading` | rename | `ltrim` | rename to `ltrim`: the parser lowers trim(LEADING ...) to `trim_leading` (cypherlite/cypherlite-parser/src/parser/expressions.rs:747), so the spelling moves to UNADVERTISED_FUNCTIONS (binder-known for the lowered form, never advertised or listed); a user-written `trim_leading(...)` stops being a catalogue name |
| `trim_trailing` | rename | `rtrim` | rename to `rtrim`: the parser lowers trim(TRAILING ...) to `trim_trailing` (cypherlite/cypherlite-parser/src/parser/expressions.rs:748), so the spelling moves to UNADVERTISED_FUNCTIONS (binder-known for the lowered form, never advertised or listed); a user-written `trim_trailing(...)` stops being a catalogue name |
| `ltrim` | keep | — | keep; canonical target of the `trim_leading` rename (corpus row features/correctness/ShowFunctions.feature:160) |
| `rtrim` | keep | — | keep; canonical target of the `trim_trailing` rename (corpus row features/correctness/ShowFunctions.feature:194) |
| `btrim` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:73 (Cypher 25 catalogue row `btrim`) |
| `replace` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:187 (Cypher 25 catalogue row `replace`) |
| `substring` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:212 (Cypher 25 catalogue row `substring`) |
| `split` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:202 (Cypher 25 catalogue row `split`) |
| `left` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:139 (Cypher 25 catalogue row `left`) |
| `right` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:190 (Cypher 25 catalogue row `right`) |
| `abs` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:63 (Cypher 25 catalogue row `abs`) |
| `ceil` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:76 (Cypher 25 catalogue row `ceil`) |
| `floor` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:124 (Cypher 25 catalogue row `floor`) |
| `round` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:191 (Cypher 25 catalogue row `round`) |
| `sqrt` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:203 (Cypher 25 catalogue row `sqrt`) |
| `sign` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:197 (Cypher 25 catalogue row `sign`) |
| `rand` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:181 (Cypher 25 catalogue row `rand`) |
| `exists` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:121 (Cypher 25 catalogue row `exists`) |
| `count` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:97 (Cypher 25 catalogue row `count`) |
| `sum` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:214 (Cypher 25 catalogue row `sum`) |
| `avg` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:72 (Cypher 25 catalogue row `avg`) |
| `min` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:163 (Cypher 25 catalogue row `min`) |
| `max` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:162 (Cypher 25 catalogue row `max`) |
| `collect` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:91 (Cypher 25 catalogue row `collect`) |
| `stdev` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:204 (Cypher 25 catalogue row `stDev`) |
| `stdevp` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:205 (Cypher 25 catalogue row `stDevP`) |
| `percentilecont` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:170 (Cypher 25 catalogue row `percentileCont`) |
| `percentiledisc` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:171 (Cypher 25 catalogue row `percentileDisc`) |
| `collect_list` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:92 (Cypher 25 catalogue row `collect_list`) |
| `stdev_samp` | keep | — | keep; canonical advertised spelling and dispatch target of the `stddev_samp` gate/rename (corpus row features/correctness/ShowFunctions.feature:208) |
| `stdev_pop` | keep | — | keep; canonical advertised spelling and dispatch target of the `stddev_pop` gate/rename (corpus row features/correctness/ShowFunctions.feature:207) |
| `stddev_samp` | gate | `gql-stddev-aliases`, canonical `stdev_samp` | gate (conflict `stddev_samp/stddev_pop`): the canonical advertised spelling is `stdev_samp` (corpus row features/correctness/ShowFunctions.feature:208); `stddev_samp` stays callable and SHOW-listed only under CYPHERLITE_PREVIEW=gql-stddev-aliases so CIP-245 keeps its 78/78 in the gated phase |
| `stddev_pop` | gate | `gql-stddev-aliases`, canonical `stdev_pop` | gate (conflict `stddev_samp/stddev_pop`): the canonical advertised spelling is `stdev_pop` (corpus row features/correctness/ShowFunctions.feature:207); `stddev_pop` stays callable and SHOW-listed only under CYPHERLITE_PREVIEW=gql-stddev-aliases so CIP-245 keeps its 78/78 in the gated phase |
| `percentile_cont` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:172 (Cypher 25 catalogue row `percentile_cont`) |
| `percentile_disc` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:173 (Cypher 25 catalogue row `percentile_disc`) |
| `nodes` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:164 (Cypher 25 catalogue row `nodes`) |
| `relationships` | keep | — | keep; canonical target of the `rels` rename (corpus row features/correctness/ShowFunctions.feature:186) |
| `rels` | rename | `relationships` | rename to `relationships`: no corpus call site for `rels(`; drop the spelling from KNOWN_FUNCTIONS and merge its metadata arm into the `relationships` row (cypherlite/cypherlite-executor/src/function_registry.rs:365) |
| `startnode` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:206 (Cypher 25 catalogue row `startNode`) |
| `endnode` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:120 (Cypher 25 catalogue row `endNode`) |
| `sin` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:198 (Cypher 25 catalogue row `sin`) |
| `cos` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:93 (Cypher 25 catalogue row `cos`) |
| `tan` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:216 (Cypher 25 catalogue row `tan`) |
| `asin` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:69 (Cypher 25 catalogue row `asin`) |
| `acos` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:64 (Cypher 25 catalogue row `acos`) |
| `atan` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:70 (Cypher 25 catalogue row `atan`) |
| `atan2` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:71 (Cypher 25 catalogue row `atan2`) |
| `log` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:157 (Cypher 25 catalogue row `log`) |
| `log10` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:158 (Cypher 25 catalogue row `log10`) |
| `exp` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:122 (Cypher 25 catalogue row `exp`) |
| `cot` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:95 (Cypher 25 catalogue row `cot`) |
| `cosh` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:94 (Cypher 25 catalogue row `cosh`) |
| `coth` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:96 (Cypher 25 catalogue row `coth`) |
| `sinh` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:200 (Cypher 25 catalogue row `sinh`) |
| `tanh` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:217 (Cypher 25 catalogue row `tanh`) |
| `haversin` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:131 (Cypher 25 catalogue row `haversin`) |
| `isnan` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:135 (Cypher 25 catalogue row `isNaN`) |
| `timestamp` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:229 (Cypher 25 catalogue row `timestamp`) |
| `degrees` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:111 (Cypher 25 catalogue row `degrees`) |
| `radians` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:180 (Cypher 25 catalogue row `radians`) |
| `typename` | rename | `valueType` | rename to `valueType` (conflict `typename`): `type` is rejected as the target because it is the RELATIONSHIP-type function; `valueType` already carries the same value_type_name implementation (eval/function/scalar.rs:38-41) |
| `date` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:98 (Cypher 25 catalogue row `date`) |
| `date.transaction` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:101 (Cypher 25 catalogue row `date.transaction`) |
| `date.statement` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:100 (Cypher 25 catalogue row `date.statement`) |
| `date.realtime` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:99 (Cypher 25 catalogue row `date.realtime`) |
| `datetime` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:103 (Cypher 25 catalogue row `datetime`) |
| `datetime.transaction` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:108 (Cypher 25 catalogue row `datetime.transaction`) |
| `datetime.statement` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:107 (Cypher 25 catalogue row `datetime.statement`) |
| `datetime.realtime` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:106 (Cypher 25 catalogue row `datetime.realtime`) |
| `localdatetime` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:147 (Cypher 25 catalogue row `localdatetime`) |
| `localdatetime.transaction` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:150 (Cypher 25 catalogue row `localdatetime.transaction`) |
| `localdatetime.statement` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:149 (Cypher 25 catalogue row `localdatetime.statement`) |
| `localdatetime.realtime` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:148 (Cypher 25 catalogue row `localdatetime.realtime`) |
| `localtime` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:152 (Cypher 25 catalogue row `localtime`) |
| `localtime.transaction` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:155 (Cypher 25 catalogue row `localtime.transaction`) |
| `localtime.statement` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:154 (Cypher 25 catalogue row `localtime.statement`) |
| `localtime.realtime` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:153 (Cypher 25 catalogue row `localtime.realtime`) |
| `time` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:224 (Cypher 25 catalogue row `time`) |
| `time.transaction` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:227 (Cypher 25 catalogue row `time.transaction`) |
| `time.statement` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:226 (Cypher 25 catalogue row `time.statement`) |
| `time.realtime` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:225 (Cypher 25 catalogue row `time.realtime`) |
| `duration` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:112 (Cypher 25 catalogue row `duration`) |
| `format` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:125 (Cypher 25 catalogue row `format`) |
| `point` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:175 (Cypher 25 catalogue row `point`) |
| `distance` | remove | — | remove (conflict `distance`): the canonical spelling of the same computation is `point.distance`; `distance` is a removed function in the corpus (SemanticErrorPart3.feature:831-835 pins 42N48), so it also drops out of the spatial seek candidate (cypherlite/cypherlite-executor/src/exec/candidates/seek.rs:235) |
| `point.distance` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:176 (Cypher 25 catalogue row `point.distance`) |
| `vector` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:254 (Cypher 25 catalogue row `vector`) |
| `vector_dimension_count` | keep | — | keep; canonical target of the `vector.dimensions` rename (corpus row features/correctness/ShowFunctions.feature:257) |
| `vector.similarity.cosine` | keep | — | keep; canonical target of the `vector.cosinesimilarity` rename (corpus row features/correctness/ShowFunctions.feature:255) |
| `vector.cosinesimilarity` | rename | `vector.similarity.cosine` | rename to `vector.similarity.cosine`: no corpus call site; drop the alias from KNOWN_FUNCTIONS and from the shared dispatch arm (cypherlite/cypherlite-executor/src/eval/function.rs:727) |
| `vector.similarity.euclidean` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:256 (Cypher 25 catalogue row `vector.similarity.euclidean`) |
| `vector.dimensions` | rename | `vector_dimension_count` | rename to `vector_dimension_count`: the callable alias goes; the `vector.dimensions` string survives ONLY as the DDL `indexConfig` option key (cypherlite/cypherlite-executor/src/vector_similarity.rs:77, cypherlite/cypherlite-ontology/src/validate/ext.rs:61) — never remove the option key |
| `vector.normalize` | rename | `vector_norm` | rename to `vector_norm`: no corpus call site; drop the callable alias |
| `vector.add` | remove | — | remove: absent from the corpus catalogue with zero call sites and no CIP behind it |
| `vector.subtract` | remove | — | remove: absent from the corpus catalogue with zero call sites and no CIP behind it |
| `vector.manhattan` | rename | `vector_distance(v1, v2, MANHATTAN)` | rename to `vector_distance(v1, v2, MANHATTAN)`: the implementation is already vector_distance_raw(Manhattan); the legacy convenience name goes, the metric-literal call stays |
| `vector.euclideandistance` | rename | `vector_distance(v1, v2, EUCLIDEAN)` | rename to `vector_distance(v1, v2, EUCLIDEAN)`: the implementation is already vector_distance_raw(Euclidean); the legacy convenience name goes |
| `vector.dotproduct` | rename | `vector_distance(v1, v2, DOT)` | rename to `vector_distance(v1, v2, DOT)`: the implementation is already vector_distance_raw(Dot); no corpus call site for the legacy name |
| `vector_distance` | keep | — | keep; canonical target of the `vector.manhattan`/`vector.euclideandistance`/`vector.dotproduct` renames (corpus row features/correctness/ShowFunctions.feature:258) |
| `vector_norm` | keep | — | keep; canonical target of the `vector.normalize` rename (corpus row features/correctness/ShowFunctions.feature:259) |
| `tofloatlist` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:234 (Cypher 25 catalogue row `toFloatList`) |
| `tointegerlist` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:237 (Cypher 25 catalogue row `toIntegerList`) |
| `point.withinbbox` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:177 (Cypher 25 catalogue row `point.withinBBox`) |
| `point.withindistance` | remove | — | remove: absent from the corpus catalogue with zero call sites; `point.withinBBox` is the surviving spatial predicate |
| `datetime.fromepoch` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:104 (Cypher 25 catalogue row `datetime.fromepoch`) |
| `datetime.fromepochmillis` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:105 (Cypher 25 catalogue row `datetime.fromepochmillis`) |
| `date.truncate` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:102 (Cypher 25 catalogue row `date.truncate`) |
| `datetime.truncate` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:109 (Cypher 25 catalogue row `datetime.truncate`) |
| `localdatetime.truncate` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:151 (Cypher 25 catalogue row `localdatetime.truncate`) |
| `localtime.truncate` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:156 (Cypher 25 catalogue row `localtime.truncate`) |
| `time.truncate` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:228 (Cypher 25 catalogue row `time.truncate`) |
| `duration.between` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:113 (Cypher 25 catalogue row `duration.between`) |
| `duration.inmonths` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:115 (Cypher 25 catalogue row `duration.inMonths`) |
| `duration.indays` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:114 (Cypher 25 catalogue row `duration.inDays`) |
| `duration.inseconds` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:116 (Cypher 25 catalogue row `duration.inSeconds`) |
| `pi` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:174 (Cypher 25 catalogue row `pi`) |
| `e` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:118 (Cypher 25 catalogue row `e`) |
| `none` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:165 (Cypher 25 catalogue row `none`) |
| `any` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:68 (Cypher 25 catalogue row `any`) |
| `all` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:65 (Cypher 25 catalogue row `all`) |
| `single` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:199 (Cypher 25 catalogue row `single`) |
| `reduce` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:185 (Cypher 25 catalogue row `reduce`) |
| `shortestpath` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:196 (Cypher 25 catalogue row `shortestPath`) |
| `allshortestpaths` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:67 (Cypher 25 catalogue row `allShortestPaths`) |
| `tobooleanornull` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:232 (Cypher 25 catalogue row `toBooleanOrNull`) |
| `tofloatornull` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:235 (Cypher 25 catalogue row `toFloatOrNull`) |
| `tointegerornull` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:238 (Cypher 25 catalogue row `toIntegerOrNull`) |
| `tostringornull` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:242 (Cypher 25 catalogue row `toStringOrNull`) |
| `char_length` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:78 (Cypher 25 catalogue row `char_length`) |
| `character_length` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:79 (Cypher 25 catalogue row `character_length`) |
| `nullif` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:168 (Cypher 25 catalogue row `nullIf`) |
| `valuetype` | keep | — | keep; canonical target of the `typename` rename (corpus row features/correctness/ShowFunctions.feature:253) |
| `linenumber` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:141 (Cypher 25 catalogue row `linenumber`) |
| `file` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:123 (Cypher 25 catalogue row `file`) |
| `linestring` | remove | — | remove: absent from the corpus catalogue with zero call sites; the geometry constructors are not Cypher-25 surface |
| `polygon` | remove | — | remove: absent from the corpus catalogue with zero call sites; the geometry constructors are not Cypher-25 surface |
| `spatial.contains` | remove | — | remove: absent from the corpus catalogue with zero call sites; the whole spatial.* predicate family is not Cypher-25 surface |
| `spatial.within` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `spatial.intersects` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `spatial.overlaps` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `spatial.crosses` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `spatial.touches` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `spatial.area` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `spatial.length` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `spatial.centroid` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `spatial.towkt` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `spatial.fromwkt` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `spatial.togeojson` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `spatial.fromgeojson` | remove | — | remove: absent from the corpus catalogue with zero call sites |
| `string.join` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:210 (Cypher 25 catalogue row `string.join`) |
| `string.regexreplace` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:211 (Cypher 25 catalogue row `string.regexReplace`) |
| `string.indexof` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:209 (Cypher 25 catalogue row `string.indexOf`) |
| `ceiling` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:77 (Cypher 25 catalogue row `ceiling`) |
| `ln` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:142 (Cypher 25 catalogue row `ln`) |
| `local_time` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:145 (Cypher 25 catalogue row `local_time`) |
| `local_datetime` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:143 (Cypher 25 catalogue row `local_datetime`) |
| `zoned_time` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:262 (Cypher 25 catalogue row `zoned_time`) |
| `zoned_datetime` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:260 (Cypher 25 catalogue row `zoned_datetime`) |
| `duration_between` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:117 (Cypher 25 catalogue row `duration_between`) |
| `path_length` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:169 (Cypher 25 catalogue row `path_length`) |
| `coll.flatten` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:82 (Cypher 25 catalogue row `coll.flatten`) |
| `coll.sort` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:89 (Cypher 25 catalogue row `coll.sort`) |
| `apoc.coll.sort` | remove (amended 2026-11-04, §5.3) | — | deleted outright (LABS-912): no registry row, no executor alias, no `apoc-coll-sort` preview gate and no dialect gate. The binder's ordinary unknown-function rejection (42N48) fires in every dialect; CIP-150 leaves `GATED_CIPS` and its re-pinned scenario [5] passes. Original disposition was `gate`; see §5.3 for the superseded text |
| `coll.indexof` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:84 (Cypher 25 catalogue row `coll.indexOf`) |
| `coll.insert` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:85 (Cypher 25 catalogue row `coll.insert`) |
| `coll.remove` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:88 (Cypher 25 catalogue row `coll.remove`) |
| `coll.max` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:86 (Cypher 25 catalogue row `coll.max`) |
| `coll.min` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:87 (Cypher 25 catalogue row `coll.min`) |
| `coll.distinct` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:81 (Cypher 25 catalogue row `coll.distinct`) |
| `coll.zip` | gate | `coll-zip` | gate (conflict `coll.zip`): callable and SHOW-listed only under CYPHERLITE_PREVIEW=coll-zip at name admission (front validate/functions.rs) and in the filtered function_catalog_rows(preview); the default dialect rejects it as an unknown function (42N48) with no catalogue row |
| `coll.hasanyof` | gate | `coll-has` | gate (conflict `coll.hasanyof`): callable and SHOW-listed only under CYPHERLITE_PREVIEW=coll-has at name admission (front validate/functions.rs) and in the filtered function_catalog_rows(preview); the default dialect rejects it as an unknown function (42N48) with no catalogue row |
| `coll.hasallof` | gate | `coll-has` | gate (conflict `coll.hasallof`): callable and SHOW-listed only under CYPHERLITE_PREVIEW=coll-has at name admission (front validate/functions.rs) and in the filtered function_catalog_rows(preview); the default dialect rejects it as an unknown function (42N48) with no catalogue row |
| `coll.hasnoneof` | gate | `coll-has` | gate (conflict `coll.hasnoneof`): callable and SHOW-listed only under CYPHERLITE_PREVIEW=coll-has at name admission (front validate/functions.rs) and in the filtered function_catalog_rows(preview); the default dialect rejects it as an unknown function (42N48) with no catalogue row |
| `cardinality` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:75 (Cypher 25 catalogue row `cardinality`) |
| `property_exists` | keep | — | keep as-is; features/correctness/ShowFunctions.feature:179 (Cypher 25 catalogue row `property_exists`) |
| `graph.byname` | keep | — | keep; stays unadvertised in the binder and catalog-only through GRAPH_SELECTOR_FUNCTIONS (cypherlite/cypherlite-core/src/namespace.rs:98-103) — the corpus row is a USE-clause selector, not a callable |
| `string.interp` | keep | — | keep; stays in UNADVERTISED_FUNCTIONS — it is the CIP-261 interpolated-string literal's lowered form, never a user-written call |
| `trim_spec` | keep | — | keep; stays in UNADVERTISED_FUNCTIONS — it is the comma-form trim lowering's internal call |
| `toBooleanList` | add | `toBooleanList` | add: lowercase registry row `tobooleanlist` (SHOW display `toBooleanList` via the row's `display` spelling, §11; corpus row features/correctness/ShowFunctions.feature:231, category List) plus its convert dispatch — it is the list form, not an alias of toBoolean |
| `allreduce` | add | `allReduce` | add: lowercase registry row `allreduce` (SHOW display `allReduce` via the row's `display` spelling, §11; corpus row features/correctness/ShowFunctions.feature:66, category Predicate, CIP-217); parsing and evaluation already exist, so only the registry row plus its metadata is missing |
| `db.indexes` | remove | — | remove: this row is the procedure, not a function — the change lands in procedure_rows (delete the registry row); the corpus reaches the surface through SHOW INDEXES |
| `db.constraints` | remove | — | remove: this row is the procedure, not a function — the change lands in procedure_rows (delete the registry row); the corpus reaches the surface through SHOW CONSTRAINTS |

### 9. Procedure disposition (138 rows)

Every row of the alignment policy, verbatim. `Mode` is the target state:
`executable` (callable), `catalog-only` (listed with `executable(false)`, never
runs) or `absent` (no engine row — absence is the aligned state).

| Name | Class | Action | Mode | Listed | Executable | Detail |
| --- | --- | --- | --- | --- | --- | --- |
| `db.info` | driver-compat | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:102; description missing today — fill it from show_expectations |
| `db.ping` | driver-compat | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:105 |
| `db.labels` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:109 |
| `db.relationshipTypes` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:113; description missing today — fill it from show_expectations |
| `db.propertyKeys` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:116; description missing today — fill it from show_expectations |
| `db.indexes` | language | remove | absent | no | no | remove the registry row (cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:119): absent from the Cypher-25 procedure inventory and from the whole corpus; CALL db.indexes() then fails as an unknown procedure and SHOW PROCEDURES stops listing it. SHOW INDEXES is the surface (ADR-0116's command set) |
| `db.constraints` | language | remove | absent | no | no | remove the registry row (cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:121): absent from the Cypher-25 procedure inventory and from the whole corpus; CALL db.constraints() then fails as an unknown procedure. SHOW CONSTRAINTS is the surface |
| `db.schema.nodeTypeProperties` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:123 |
| `db.schema.relTypeProperties` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:133 |
| `db.schema.visualization` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:152 |
| `db.createLabel` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:165; description missing today — fill it from show_expectations; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `db.ontology.set` | language | keep | executable | yes | yes | keep; unchanged, still preview-gated behind CYPHERLITE_PREVIEW=ontology (ADR-0378) |
| `db.ontology.get` | language | keep | executable | yes | yes | keep; unchanged, still preview-gated behind CYPHERLITE_PREVIEW=ontology (ADR-0378) |
| `db.ontology.drop` | language | keep | executable | yes | yes | keep; unchanged, still preview-gated behind CYPHERLITE_PREVIEW=ontology (ADR-0378) |
| `db.ontology.apply` | language | keep | executable | yes | yes | keep; unchanged, still preview-gated behind CYPHERLITE_PREVIEW=ontology (ADR-0378) |
| `db.awaitIndex` | language | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:202; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus; the corpus pins its description and signature verbatim ('Wait for an index to come online (for example: CALL db.awaitIndex("MyIndex", 300)).', 'db.awaitIndex(indexName :: STRING, timeOutSeconds = 300 :: INTEGER)') at features/correctness/ShowProcedures.feature:213,224 |
| `db.awaitIndexes` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:209; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `db.resampleIndex` | language | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:212; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `db.index.fulltext.queryNodes` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:218; description missing today — fill it from show_expectations |
| `db.index.fulltext.queryRelationships` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:229; description missing today — fill it from show_expectations |
| `db.index.vector.queryNodes` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:241 |
| `db.index.vector.queryRelationships` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:249 |
| `dbms.setConfigValue` | db-management | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:257; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `cdc.current` | language | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:267 |
| `cdc.earliest` | language | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:272 |
| `cdc.query` | language | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:277 |
| `dbms.cluster.recreateDatabase` | db-management | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:292; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `dbms.cluster.routing.getRoutingTable` | db-management | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:298; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `db.stats.collect` | db-management | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:306; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `dbms.routing.getRoutingTable` | db-management | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:320; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `db.cdc.current` | language | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:327 |
| `db.index.fulltext.awaitEventuallyConsistentIndexRefresh` | language | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:334; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `db.index.fulltext.listAvailableAnalyzers` | language | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:339 |
| `db.checkpoint` | db-management | keep | catalog-only | yes | no | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:350; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `tx.setMetaData` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:359; description missing today — fill it from show_expectations; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `tx.getMetaData` | language | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:362; description missing today — fill it from show_expectations; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `dbms.components` | driver-compat | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:366; description missing today — fill it from show_expectations; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `dbms.info` | driver-compat | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:378; description missing today — fill it from show_expectations; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `dbms.showCurrentUser` | driver-compat | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:384; description missing today — fill it from show_expectations; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `dbms.clientConfig` | driver-compat | keep | executable | no | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:393; description missing today — fill it from show_expectations |
| `dbms.listConfig` | driver-compat | keep | executable | yes | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:396; description missing today — fill it from show_expectations; align the drifted metadata (mode/worksOnSystem/listed, named in the research evidence) to the corpus |
| `dbms.licenseAgreementDetails` | driver-compat | keep | executable | no | yes | keep; registry cypherlite/cypherlite-executor/src/procedure_registry/signatures.rs:399; description missing today — fill it from show_expectations |
| `db.stats.retrieve` | language | add | executable | yes | yes | add an executable READ procedure `db.stats.retrieve(section :: STRING) :: (section :: STRING, data :: MAP)` implementing the GRAPH COUNTS section from the engine's live constraints/indexes/graph types; the corpus calls it 7x (features/correctness/GraphCounts.feature:38 and siblings) and all 6 executed scenarios fail today with ProcedureNotFound |
| `changedProc` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `db.cdc.earliest` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.cdc.query` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.cdc.translateId` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.clearQueryCaches` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.create.setNodeVectorProperty` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.create.setRelationshipVectorProperty` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.createProperty` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.createRelationshipType` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.listLocks` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.prepareForReplanning` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.resampleOutdatedIndexes` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.stats.clear` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.stats.retrieveAllAnonymized` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.stats.status` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `db.stats.stop` | language | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.checkConfigValue` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.cluster.checkConnectivity` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.cluster.cordonServer` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.cluster.deallocateDatabaseFromServer` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.cluster.deallocateDatabaseFromServers` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.cluster.deallocateNumberOfDatabases` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.cluster.protocols` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.cluster.reallocateDatabase` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.cluster.reallocateNumberOfDatabases` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.cluster.secondaryReplicationDisable` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.cluster.setAutomaticallyEnableFreeServers` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.cluster.statusCheck` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.killConnection` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.killConnections` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.listActiveLocks` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.listCapabilities` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.listConnections` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.listPools` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.promoteReplicaDatabase` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.queryJmx` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.recreateDatabase` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.reloadProcedures` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.scheduler.failedJobs` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.scheduler.groups` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.scheduler.jobs` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.security.clearAuthCache` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.security.reloadTLS` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.setDefaultAllocationNumbers` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.setDefaultDatabase` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.showTopologyGraphConfig` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `dbms.unquarantineDatabase` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `fleetManagement.debugLogging` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `fleetManagement.disable` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `fleetManagement.inspectToken` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `fleetManagement.registerToken` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `fleetManagement.reportedData` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `fleetManagement.restart` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `fleetManagement.status` | db-management | add | catalog-only | yes | no | add a listing-only row (`ProcSignature::read(...).executable(false)`, listed): the Cypher-25 inventory lists it, no corpus scenario CALLs it, and the engine has no such operation to run — so SHOW PROCEDURES matches without inventing execution |
| `newProc` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `oldProc` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `oldProcNoReplacement` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `procWithOutput` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `apoc.meta.schema` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `apoc.meta.stats` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `db.nestedQueryProcedure` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `dbms.create_a_node` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `getNode` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `internal.db.system.info` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `internal.virtual_graph.ping` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `my.first.proc` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `my.first.value` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.aNodeWithLabel` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.createNodeWithLoop` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.dbmsWithFailure` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.expandNode` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.findNodesWithLabel` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.graphAlgosDijkstra` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.internalTypes` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.procWithDepr` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.readWithFailure` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.recurseN` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.schemaWithFailure` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.stream123` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.stream123Depr` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.time` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.voidProc` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `org.neo4j.writeWithFailure` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `proc.two` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `proc.void` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `proc.yielding` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `procWithWarning` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `something.with.deprecated.output` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `test.doNothing` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `test.labels` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `test.my.proc` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `test.rate` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `user.failWithDatabaseErrorWhen` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `user.failWithFatalErrorWhen` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |
| `user.failWithTransientErrorWhen` | language | keep | absent | no | no | keep-absent: corpus test-fixture procedure, not an engine built-in; CypherLite exposes no procedure-registration surface (steps/cypher_steps.py:60-66 skips the registration step), so the aligned state is absence — no row, no listing, no CALL |

### 10. Residual scope

The alignment fixes names, spellings, modes, flags, metadata and signatures. It
does not implement `SHOW PROCEDURES EXECUTABLE` / `EXECUTABLE BY` parsing: the
93-row executable table is asserted through that clause, so those scenarios
stay harness-gap until it is parsed. That work is recorded as the verify node's
residual, not folded in here.

The single-row catalogue skew is also residual: the corpus catalogue's 177
distinct names minus the gated-off `coll.zip` row is 176 default-listed names,
so the whole-catalogue, count and full-ordering scenarios
(`ShowFunctions.feature:61`, `:201`, `:2221`, `:2438`) stay harness-gap until
CIP-274 ships. Enabling `coll-zip` restores the row.

### 11. Display spellings (amended 2026-11-04)

Function calls match case-insensitively (CIP-230), so `KNOWN_FUNCTIONS` and
every admission key stay lowercase (§4); the executor's metadata table below
matches each row's name case-insensitively instead. The user-facing spelling is a
separate fact: a reference server prints 42 of the 176 advertised names in
mixed case. The casing reference is **Neo4j 2026.10.1**, read from a live
`SHOW FUNCTIONS YIELD name` and cross-checked against the corpus's generated
`ShowFunctions*` tables -- the same first-tier authority as the names
themselves (§1). Case-insensitively the advertised set already equalled the
reference's; only the display case differed.

- **One display fact per row.** A registry row's own `name` field
  (`FunctionMeta`, `cypherlite/cypherlite-executor/src/function_registry.rs`)
  *is* its display spelling: the 42 recased names carry their reference casing
  as the row's name, every other name is the lowercase spelling, and the single
  renderer (`show_catalog::function_row`) names each row from its own `name`
  for both the `name` cell and the CIP-113 signature -- the hand-written
  `special_signature` rows store only the signature tail after the name, so
  the row and its signature cannot disagree. No second name column exists
  beside the row's name.
- **Matching is untouched.** A display spelling is never an admission rule:
  `function_metas` compares the call name against each row's name
  case-insensitively, and the binder still lowercases the call name, so
  `ToBoolean('true')`, `tOuPpEr('x')` and `STDEV(x)` all bind exactly as the
  lowercase forms do.
- **The set is pinned by test.** `show_catalog`'s display test asserts the
  catalog's mixed-case names are exactly the 42 rows below -- plus the three
  preview-gated `coll.has*` rows, rendered in their CIP camelCase -- one
  entry per rendered row, and that every row's signature cell starts with its
  own name. The registry-coverage test asserts every row name lowercases into
  the advertised set, so a row can never name a different function, and
  `function_registry`'s alias test ties the four recased aggregate names to
  `AGGREGATE_ALIASES`' display column, the spelling the lowering emits.
- **Gated families.** `coll.zip` and the `gql-stddev-aliases` rows keep their
  registered spelling: CIP-274 and CIP-245 spell them lowercase, so neither is
  a reference spelling to recase. `graph.names` stays lowercase as well — the
  reference agrees, and only the three other catalog-only `graph.*` rows and
  `db.nameFromElementId` take mixed case.

| Admitted (lowercase) name | Registry row name (rendered spelling) |
| --- | --- |
| `allreduce` | `allReduce` |
| `allshortestpaths` | `allShortestPaths` |
| `coll.indexof` | `coll.indexOf` |
| `db.namefromelementid` | `db.nameFromElementId` |
| `duration.indays` | `duration.inDays` |
| `duration.inmonths` | `duration.inMonths` |
| `duration.inseconds` | `duration.inSeconds` |
| `elementid` | `elementId` |
| `endnode` | `endNode` |
| `graph.byelementid` | `graph.byElementId` |
| `graph.byname` | `graph.byName` |
| `graph.propertiesbyname` | `graph.propertiesByName` |
| `isempty` | `isEmpty` |
| `isnan` | `isNaN` |
| `nullif` | `nullIf` |
| `percentilecont` | `percentileCont` |
| `percentiledisc` | `percentileDisc` |
| `point.withinbbox` | `point.withinBBox` |
| `randomuuid` | `randomUUID` |
| `shortestpath` | `shortestPath` |
| `startnode` | `startNode` |
| `stdev` | `stDev` |
| `stdevp` | `stDevP` |
| `string.indexof` | `string.indexOf` |
| `string.regexreplace` | `string.regexReplace` |
| `toboolean` | `toBoolean` |
| `tobooleanlist` | `toBooleanList` |
| `tobooleanornull` | `toBooleanOrNull` |
| `tofloat` | `toFloat` |
| `tofloatlist` | `toFloatList` |
| `tofloatornull` | `toFloatOrNull` |
| `tointeger` | `toInteger` |
| `tointegerlist` | `toIntegerList` |
| `tointegerornull` | `toIntegerOrNull` |
| `tolower` | `toLower` |
| `tostring` | `toString` |
| `tostringlist` | `toStringList` |
| `tostringornull` | `toStringOrNull` |
| `toupper` | `toUpper` |
| `uuid.leastsignificantbits` | `uuid.leastSignificantBits` |
| `uuid.mostsignificantbits` | `uuid.mostSignificantBits` |
| `valuetype` | `valueType` |

## Alternatives considered

- **Keep the current inventory and fix only the catalogue metadata.** Rejected:
  the corpus's whole-catalogue, count and ordering scenarios pin names and
  spellings, so metadata alone leaves the nineteen removed names and the ten
  renamed-away spellings advertised and two names missing (`toBooleanList`,
  `allReduce`); the divergence itself is the defect.
- **Apply LABS-874's list literally** (`typename` -> `type`, `distance` ->
  `vector_distance`, `stddev_*` -> plain rename). Rejected: `type` is the
  RELATIONSHIP-type function (catalogue row 246), so that rename silently
  changes every existing `type()` call; `vector_distance` is a metric-taking
  vector function, a false alias for point distance; and a plain `stddev`
  rename drops ten frozen CIP-245 scenarios. The corpus settles all five
  conflicts (§5).
- **Re-pin CIP-245 and CIP-150** — drop the `@doc`-derived scenarios and lower
  `baselines/cip-eval.json`. Rejected: re-pinning is out of scope for this
  feature and the cip-eval ratchet only moves up (ADR-0001); lowering a frozen
  baseline trades a catalogue fix for an evaluation regression, while the
  preview gate keeps both.
- **Delete the CIP-274/CIP-286 implementations and their testdata.** Rejected:
  both are approved CIPs with passing ratchets (13/13 and 44/44); deleting
  verified work needs the same baseline edits as re-pinning and throws away
  surface users can already opt into.
- **Advertise `coll.zip` and the `coll.has*` trio unconditionally**, following
  the catalogue row at `ShowFunctions.feature:90`. Rejected:
  the corpus's function-expressions acceptance rows record the live probe
  — front-end registered but unreachable, no `SHOW` row, 42N48 under CYPHER 5
  and 25 — so advertising claims a name no released server has; the gate keeps
  the CIP surface available without the false claim.
- **Fold the stddev and apoc names into one of the existing flags** (reuse
  `coll-zip` and friends). Rejected: one flag names one capability; folding
  unrelated CIPs together makes one opt-in silently enable surface the caller
  did not ask for, and breaks the registry's one-row-one-name contract.
- **Add a second env var or a per-surface option for function names** instead
  of extending ADR-0371. Rejected: ADR-0371 rejected that shape for syntax --
  N flags, no registry, silent typos, per-surface documentation — and the same
  reasons hold for names; the registry already owns the spelling, the list
  parse and the fail-closed error.
- **Keep `db.indexes` / `db.constraints` as deprecated catalog-only rows.**
  Rejected: the corpus has no row and no call site, so a deprecated row invents
  a name and would claim a Neo4j deprecation that was never announced;
  ADR-0116's `SHOW INDEXES` / `SHOW CONSTRAINTS` are the surface.
- **Store the mixed-case spellings in `KNOWN_FUNCTIONS` itself and lowercase at
  every lookup.** Rejected: the registry is the *matching* fact, consulted by
  the binder and the preview-gated name table, keyed by the lowercase
  spelling; recasing it would move a rendering concern into the admission path
  and force every one of those lookups to re-normalize. The executor's
  metadata-table row names (§11) keep the casing as one rendering fact read by
  one renderer, matched case-insensitively and never consulted for admission.

## Consequences

- Default-dialect functions: 176 known names after the alignment — the 172
  names admitted by default plus the four catalog-only `graph.*` rows — with
  19 names removed, 10 renamed away and 2 added (`tobooleanlist`, `allreduce`),
  and the six gated entries admitting only under their flags, each listed
  there too. `UNADVERTISED_FUNCTIONS` goes 3 -> 5
  (`trim_leading`/`trim_trailing` join the lowering internals). `SHOW FUNCTIONS`
  therefore lists those 176 distinct names, the corpus catalogue's 177 minus
  the gated-off `coll.zip` row. That single-row skew is the alignment's only
  residual catalogue gap (§10).
- Default-dialect procedures: 2 rows removed, 53 listing-only rows added, 1
  executable row added (`db.stats.retrieve`), 6 rows become listed, 42 corpus
  test-fixture names stay absent, and the other 40 registry rows keep their
  mode (26 executable, 14 catalog-only) with their metadata aligned to the
  corpus. The registry grows from 42 rows to 94 (27 executable, 67
  catalog-only).
- cip-eval: CIP-274, CIP-286, CIP-245 and CIP-150 joined `GATED_CIPS` (four
  rows); CIP-150 left again on 2026-11-04 with the `apoc.coll.sort` deletion
  (§5.3) and its re-pinned scenario [5] added an `ENGINE_PASSING.json`
  identity. `baselines/cip-eval.json` is not lowered: CIP-245 78/78, CIP-150
  28/28, CIP-274 13/13 and CIP-286 44/44 all keep passing.
- Preview registry: four rows were appended to `PREVIEW_FEATURE_TABLE`, four
  `Ok(false)` arms added to the front's exhaustive gate-loop match, one
  `PREVIEW_FUNCTIONS` row per gated name, and `validate_function_admission`'s
  pre-binder walk rejecting a disabled name with the ordinary unknown-function
  error. `AGGREGATE_ALIASES` is untouched — no per-spelling gate exists,
  because the walk reads the parsed spellings before lowering consults the
  alias table. Each row is temporary by construction -- §3's removal
  conditions — and the live gated-feature list grew from twelve to sixteen
  (fifteen after the 2026-11-04 amendment below).
- Amended 2026-11-04 (LABS-912): `apoc-coll-sort` is deleted outright (§5.3),
  so `PREVIEW_FEATURE_TABLE` carries three ADR-0497 rows (fifteen live gated
  features), CIP-150 leaves `GATED_CIPS` and runs in the ungated phase, and
  its re-pinned scenario [5] joins `ENGINE_PASSING.json`; CIP-150's baseline
  count is unchanged at 28/28.
- Amended 2026-11-04 (LABS-912, display spellings): `SHOW FUNCTIONS` renders
  the reference catalogue's casing — 42 default names in mixed case plus the
  three gated `coll.has*` rows (§11) — in both the `name` cell and the CIP-113
  signature. Matching is unchanged: every name still binds in any casing,
  because admission stays lowercase and the metadata table's row names (the
  display spellings) are matched case-insensitively — a rendering fact, never
  an admission rule. `docs/reference/functions.md` and the `entity-resolution`
  cookbook quote the aligned spellings.
- Removed and renamed names are breaking: a query calling `distance`, the
  `spatial.*` family, `vector.add`/`subtract`/`dotProduct`/`manhattan`/
  `euclideanDistance`, `vector.dimensions`/`normalize`/`cosineSimilarity`,
  `typename`, `rels`, `stddev_*`, or `apoc.coll.sort` now fails as an unknown
  function (`apoc.coll.sort` was deleted outright on 2026-11-04 — §5.3);
  `trim_leading`/`trim_trailing` leave the
  advertised catalogue and stay binder-known parser lowering targets
  (`UNADVERTISED_FUNCTIONS`), so `trim(LEADING ...)` / `trim(TRAILING ...)`
  keep lowering and a user-written `trim_leading(...)` / `trim_trailing(...)`
  call still binds and executes — the internal names stop being catalogue
  surface, not callable surface. The docs that name them change in the
  cutover commits (§7).
- ADR-0371 gains a Later references note recording the function-name admission
  gate as a narrow exception to the syntax-only scope policy, and
  recording that a name gate's disabled form is the ordinary unknown-function
  rejection rather than the authored preview error. ADR-0378's procedure-family
  row is unchanged and is cited here as the structural precedent
  (`procedure_preview_feature` plus a preview-filtered `SHOW` catalogue is the
  pattern the function gate copies); ADR-0116's `SHOW`-command surface is cited
  for the `db.indexes` / `db.constraints` removal.
- This commit is documentation only: the ADR and ADR-0371's Later references
  note. No code, test or doc-reference change rides with it; the cutover nodes
  land the registry, gate, catalogue, procedure and documentation changes the
  sections above specify.

## Later references

- ADR-0542 records a fifth admission class, engine-only, callable, unlisted:
  a name outside the corpus that works in every build but has no `SHOW
  FUNCTIONS` row. It is a recorded exception to the four rules in §2, not an
  amendment of them.
