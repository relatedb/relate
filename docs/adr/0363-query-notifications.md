# ADR-0363: Query notifications: vocabulary and transport

Status: Proposed
Claim: 43ca09bd9b545e0b54a88119f2e03eaa69b93fe6

## Context

ADR-0344/0348 gave `EXPLAIN` and `PROFILE` a structured `PlanNode` tree with
per-operator estimates, metrics and typed warnings (`EstimationMiss`,
`CartesianProduct`, `AllNodesScan{index}`). Those warnings render in the plan
table's `Warnings` column and footer and ride the plan wire form, but they are
a plan-tree detail: no driver summary carries them, and the engine has no
vocabulary for diagnostics without an operator — an unknown label,
relationship type or property key, a deprecated function or procedure, a
scope-less `CALL` subquery. LABS-355 asks for Neo4j's notification taxonomy so
drivers, Browser and agents get the hints they already render.

The facts the design starts from:

- Neo4j's notification severities are `INFORMATION` and `WARNING`; the
  categories in scope are `PERFORMANCE`, `UNRECOGNIZED` and `DEPRECATION`.
  The performance set is `CartesianProduct` (03N90),
  `UnboundedVariableLengthPattern` (03N91), `NoApplicableIndex` (03N93),
  `EagerOperator` (03N94) and `DynamicProperty` (03N95); the unrecognized
  set is `UnknownLabelWarning` (01N50), `UnknownRelationshipTypeWarning`
  (01N51) and `UnknownPropertyKeyWarning` (01N52); deprecations are
  `FeatureDeprecationWarning` under 01N00 (generic), 01N01 (with a
  replacement) and 01N02 (without).
- Bolt ≤5.4 carries `notifications` on the SUCCESS that ends a PULL/DISCARD
  (`has_more == false`), keys `code`, `title`, `description`, `severity`,
  `category`, `position` (position omitted when empty); the key is omitted
  when empty and never rides RUN SUCCESS. HTTP Query API v2 carries it with
  keys `code`, `description`, `severity`, `title`, `position`, `category`,
  `position` null when absent, and omits the key when empty.
- The engine builds a plan tree only for `EXPLAIN`/`PROFILE`
  (`explain.rs`; ADR-0344 decision 3). The plain path — including
  `Connection::plan_gate_classify`, which plans every statement but builds no
  tree — does no plan-tree work. Bolt negotiates 4.4 only (`protocol.rs`);
  HTTP's success body is `success_envelope` in `query_api.rs`.
- The existing warning payload already carries the first remedy: the
  `AllNodesScan` warning names the property index that would serve the scan. The
  deprecation facts already exist beside the catalog:
  `FunctionMeta.deprecated/deprecated_by` (`function_registry.rs`, `id()` →
  `elementId`) and `ProcSignature.deprecated/deprecated_by`
  (`procedure_registry.rs`). A scope-less `CALL { … }` is `CallScope::None`
  in the IR (`ast.rs`).
- MCP's `call_cypher` already runs `EXPLAIN <query>` on every call to
  classify the statement (`tools.rs`), so that call's plan-time
  notifications are already computed.

## Decision

1. **One vocabulary, one table.** The notification kinds, their code,
   GQLSTATUS, severity, category, title and remedy live in one module in
   `cypherlite-model` beside `plan_wire`, and the table below is that
   vocabulary. `PlanWarning`'s three variants are its operator-node subset and
   keep their names, messages, node placement and wire spelling; their text
   delegates to this table, so the plan table's warning column/footer and the notification
   list cannot drift. A notification record is
   `{ kind, description, remedy, operator_id }`: `description` carries the
   instance detail (the unknown name, the index text, the procedure name) and
   the kind supplies the metadata and the remedy.

| kind | code | GQLSTATUS | severity | category | trigger | remedy |
| --- | --- | --- | --- | --- | --- | --- |
| `CartesianProduct` | `Neo.ClientNotification.Statement.CartesianProduct` | `03N90` | INFORMATION | PERFORMANCE | A multi-pattern `MATCH` whose patterns share no node variable — the existing `PlanWarning::CartesianProduct`. | Give the patterns a join anchor: reuse a node variable across them, or add a pattern that relates their endpoints; otherwise run them as separate queries. |
| `UnboundedVariableLengthPattern` | `Neo.ClientNotification.Statement.UnboundedVariableLengthPattern` | `03N91` | INFORMATION | PERFORMANCE | A variable-length relationship pattern with no upper bound (`[:R*]`) that plans as an unbounded expansion. CypherLite fires on every such pattern, not only inside `shortestPath` (Neo4j's narrower trigger) — the cost shape is the same. | Bound the expansion (`[:R*1..5]`) where arbitrary length is not required, or add a selective endpoint/label predicate and a `LIMIT`. |
| `AllNodesScan` | `CypherLite.Notification.Statement.AllNodesScan` | `03C90` | INFORMATION | PERFORMANCE | A lone label-less node pattern (`MATCH (n)`) while the store carries a property index — the existing `PlanWarning::AllNodesScan`; the description names the first index text. | Name the label the scan intends (`MATCH (n:<Label>)`); on the index's label the scan becomes an index-backed lookup. |
| `MissingIndex` | `CypherLite.Notification.Statement.MissingIndex` | `03C91` | INFORMATION | PERFORMANCE | A label pattern with an equality property predicate (`MATCH (n:Person) WHERE n.email = $e`) and no `CREATE INDEX` range index on that `(Label, property)` pair. | `CREATE INDEX cypherlite_Person__email FOR (n:Person) ON (n.email)` — the exact runnable statement, with the name derived deterministically from label and property (case preserved; `_` escaped to `_5f`, every other non-alphanumeric byte to `_` plus two hex digits, and a double `__` separator between the escaped parts — escaped parts never contain `__`, so the split is unambiguous and distinct pairs cannot collide in the global index namespace). |
| `EagerAggregation` | `CypherLite.Notification.Statement.EagerAggregation` | `03C92` | INFORMATION | PERFORMANCE | An `EagerAggregation` operator whose input child's `estimated_rows` is at least 100,000 — the stated `EAGER_INPUT_NOTIFICATION_ROWS` threshold. | Reduce the rows reaching the aggregation with a selective label/property predicate or a `LIMIT` before it; profile again to confirm the buffered input shrank. |
| `EstimationMiss` | `CypherLite.Notification.Statement.EstimationMiss` | `03C93` | INFORMATION | PERFORMANCE | `PROFILE` only: the existing 10x overshoot policy fires (estimate at least 10x the actual rows, actual > 0) — the same policy behind the ratio cell's `!`. | Rework the operator's input so the estimate can see the filter (add the selective predicate at the scan, or reorder the join); the estimate is a planner assumption, not a runtime fact. |
| `UnknownLabelWarning` | `Neo.ClientNotification.Statement.UnknownLabelWarning` | `01N50` | WARNING | UNRECOGNIZED | A **read** node pattern names a label absent from the catalog (`db.labels`' source); a `CREATE`/`MERGE` target pattern is not evidence (decision 13). | Check the spelling; the label exists after the first write that creates it, and no index or constraint uses the unknown name. |
| `UnknownRelationshipTypeWarning` | `Neo.ClientNotification.Statement.UnknownRelationshipTypeWarning` | `01N51` | WARNING | UNRECOGNIZED | A **read** relationship pattern names a type absent from the catalog (`db.relationshipTypes`' source); a `CREATE`/`MERGE` target pattern is not evidence (decision 13). | Check the spelling; the type exists after the first write that creates it. |
| `UnknownPropertyKeyWarning` | `Neo.ClientNotification.Statement.UnknownPropertyKeyWarning` | `01N52` | WARNING | UNRECOGNIZED | A **read** pattern property predicate or read property access names a key absent from the view's interner (`ExecGraph::has_property_key`), including an explicit transaction's scratch writes, a superset of `db.propertyKeys`' distinct stored-key walk; a `SET`/`REMOVE` target key is not evidence (decisions 5 and 13). | Check the spelling; the key exists after the first write that sets it. |
| `DeprecatedFunction` | `Neo.ClientNotification.Statement.FeatureDeprecationWarning` | `01N01` | WARNING | DEPRECATION | A function call resolves to a catalog `FunctionMeta` whose `deprecated_by` is `Some` (`id()` today). | Replace the call with the catalog's replacement identifier (`id(n)` → `elementId`). |
| `DeprecatedProcedure` | `Neo.ClientNotification.Statement.FeatureDeprecationWarning` | `01N01` | WARNING | DEPRECATION | A `CALL` resolves to a `ProcSignature` whose `deprecated_by` is `Some` (e.g. `db.index.vector.queryNodes` → the `SEARCH` clause). | Use the registry's `deprecated_by` replacement; a procedure/function replacement renders as an identifier, a clause as a clause. |
| `CallSubqueryWithoutScope` | `Neo.ClientNotification.Statement.FeatureDeprecationWarning` | `01N00` | WARNING | DEPRECATION | A `CALL { … }` subquery lowers to `CallScope::None` (no variable scope clause). | Add the scope clause the body imports: `CALL (n) { … }`, or `CALL () { … }` when it imports nothing. |

2. **Namespace.** Engine-own kinds use the one root
   `CypherLite.Notification.*`, with statement-scoped kinds under
   `CypherLite.Notification.Statement.*` (Neo4j's layout, without its
   prefix). Their GQLSTATUS values put `C` in position 3 — the first
   character of the 3-character subclass, where GQL-standard codes use `G`
   and Neo4j-defined codes use `N` — so every `C` value is disjoint from
   both namespaces by construction. Engine-own values are allocated from
   `03C90` upward (performance) and `01C00` upward (warnings); none is a
   Neo4j-assigned value. Neo4j's code and GQLSTATUS are reused only where the
   diagnostic is the same — `CartesianProduct`,
   `UnboundedVariableLengthPattern`, the three `Unknown*Warning`s and
   `FeatureDeprecationWarning` — never for an engine-specific trigger
   (`AllNodesScan`, `MissingIndex`, `EagerAggregation`, `EstimationMiss`).
   No form carries a `position` key: not the engine-native record, not Bolt,
   not HTTP.

3. **Derivation is opt-in with the plan tree.** Notifications are derived
   exactly where a `PlanNode` tree is built (`plan_node::plan_tree` for
   `EXPLAIN`, `profiled_tree` for `PROFILE`) and nowhere else. The plain
   execution path does no notification work: no catalog walk, no
   property-key walk, no allocation; `QueryResult.notifications` is empty
   there. `plan_gate_classify` continues to plan every statement for
   admission and query-type classification but builds no tree, so it derives
   no list. ADR-0344's opt-in budget stays intact: a plain run pays nothing
   for a feature only `EXPLAIN`/`PROFILE` consumers read.

4. **Operator-scoped kinds.** `CartesianProduct`,
   `UnboundedVariableLengthPattern`, `AllNodesScan`, `MissingIndex`,
   `EagerAggregation` and `EstimationMiss` are derived from the tree's
   pattern, operator and estimate facts at tree construction, each carrying
   its operator's `PlanNode.id` as `operator_id`. `EstimationMiss` is
   `PROFILE`-only (it needs actual rows) and is stamped where the metrics
   are, from the same 10x policy the ratio cell's `!` reads (ADR-0348). The
   three existing warnings keep their per-node `warnings` placement; the
   newer operator-scoped kinds do not join the plan table's `Warnings`
   column (decision 8).

5. **Statement-scoped kinds.** `UnknownLabelWarning`,
   `UnknownRelationshipTypeWarning` and `UnknownPropertyKeyWarning` read the
   same facts the existing surfaces already expose — the view's catalog
   (`Catalog.labels` / `Catalog.rel_types`) and, for keys, its interner
   (`ExecGraph::has_property_key`), all read through the committed graph
   plus an explicit transaction's scratch writes — never a second
   vocabulary. Only read-side
   mentions of labels and relationship types are evidence (decision 13).
   The interner is a **superset** of `db.propertyKeys`' distinct stored-key
   walk: every stored key is interned by the CREATE/SET/MERGE property path
   and by checkpoint/WAL restore, so a **miss is a true miss** and the
   diagnostic can never warn about a key the store holds. The converse does
   not hold — labels, relationship types and the internal relationship `ts`
   key share the interned namespace, and a key removed from every entity
   stays interned — so a **hit may suppress** a warning: that is a false
   negative at worst, never a false positive. The one-sided guarantee (no
   false positives; false negatives possible) is the contract, and it is
   the reason the interner, not the `db.propertyKeys` walk, is the source.
   `DeprecatedFunction` and `DeprecatedProcedure` read the catalog's
   `deprecated_by` option (`None` means not deprecated);
   `CallSubqueryWithoutScope` reads `CallScope::None`.
   `FeatureDeprecationWarning` uses 01N01 for both deprecated kinds: the
   replacement is the whole deprecation fact, so a deprecated entry always
   names one (`id()` → `elementId`, `db.index.vector.queryNodes` → `SEARCH`).
   `function_deprecation_lookup_reads_the_function_catalog` and
   `procedure_deprecation_lookup_reads_the_registry` pin the lookups. The
   vocabulary carries no 01N02 row: a replacement-less entry would require
   adding that row (decision 1's vocabulary-addition path) in the same
   change; the detectors never downgrade a deprecated entry to the generic
   01N00. The scope-less `CALL` subquery is the generic 01N00.

6. **Order and dedup.** The list is deterministic: operator-scoped entries
   in the tree's pre-order (the `Id` column's order), then statement-scoped
   entries in table order (`UNRECOGNIZED` before `DEPRECATION`, then code,
   then detail). Identical `(kind, description)` entries collapse to the
   first. A statement has one flat list; a consumer that wants the operator
   association reads `operator_id`.

7. **Rust surface.** `QueryResult` gains
   `pub notifications: Vec<Notification>` beside `plan` (`None`/empty on a
   plain run); `Notification` is the `cypherlite-model` record above. This
   is the one source every surface projects from.

8. **CLI text output never renders a kind twice.** The
   `EXPLAIN`/`PROFILE` plan table keeps its per-operator `Warnings` column
   and its `Warnings: Id <id>: <message>; …` footer unchanged; those two are
   the only rendering of the three plan-warning kinds (`CartesianProduct`,
   `AllNodesScan`, `EstimationMiss`) **in an output that prints the plan
   table**. After the table, `table` output renders one `Notifications:` block
   with every other kind — the three `Unknown*Warning`s, `DeprecatedFunction`,
   `DeprecatedProcedure`, `CallSubqueryWithoutScope`,
   `UnboundedVariableLengthPattern`, `MissingIndex` and `EagerAggregation` —
   one line each:
   `<SEVERITY> <gql_status> <code>: <description> Remedy: <remedy>`,
   prefixed `Id <id>: ` when `operator_id` is present. `markdown` has no plan
   table, so its block carries the complete list, plan-warning kinds
   included: the partition is against what the same output renders, never
   against a table it does not print. The block is absent when that list is
   empty. The partition is by kind, so a kind's text can never appear in both
   places of one output. JSON-family modes (decision 9) carry the
   complete list once; the plan tree's own `warnings` stay a separate plan
   structure.

9. **Engine-native wire keys.** The shared success envelope emits an
   always-present `notifications` array (empty on a plain run) whose entries
   carry `code`, `gql_status`, `severity`, `category`, `title`, `description`,
   `remedy` and, only on operator-scoped kinds, `operator_id` (`gql_status` is the
   GQLSTATUS key; `description` is the diagnosis and `remedy` the action,
   separate by design). `cypherlite_model::wire::response_envelope` emits it;
   the same list rides the CLI `--mode envelope` (and the other JSON-family
   modes through `StatementMeta`), MCP's `read-cypher`/`write-cypher`
   envelopes, WASM's `execute_wire` JSON, the C ABI's `query_result_to_wire`
   JSON handle (and therefore JavaScript, Java and Go), and Python's
   `QueryResult.notifications` attribute — one result-metadata addition
   reaching every surface that can expose it in the same change.

10. **MCP reuses its classification `EXPLAIN`.** `call_cypher` already runs
    `EXPLAIN <query>` to classify the statement; the tool response carries
    `explained.notifications`. It does not run a second `EXPLAIN` and never
    runs `PROFILE`, so `EstimationMiss` (PROFILE-only) is absent there while
    every plan-time kind is present. The executed statement's empty list is
    not consulted.

11. **Bolt.** The list rides the SUCCESS that ends the result — the PULL or
    DISCARD with `has_more == false` — beside `type`, `t_first`, `t_last`,
    `db` and the `plan`/`profile` pair (`summary::ending`), never RUN
    SUCCESS. Entries use key order `code`, `title`, `description`,
    `severity`, `category`; `description` is the diagnosis with the remedy
    appended as ` (remedy: <remedy>)`; no `position` key ever appears. The
    `notifications` key is omitted when the list is empty. Bolt 4.4 is the
    only negotiated version (`protocol.rs`).

12. **HTTP Query API v2.** `success_envelope` gains `notifications` beside
    `counters` and `bookmarks`. Each entry carries Neo4j's five-key set —
    `code`, `title`, `description`, `severity`, `category` — with no
    `position` key ever emitted, because no position value exists. The
    **object key order is not part of the HTTP contract**: the serializer
    builds a default `serde_json::Map` (a `BTreeMap`; the workspace does not
    enable `preserve_order`), so keys serialize alphabetically as
    `category`, `code`, `description`, `severity`, `title`. JSON object key
    order is insignificant on the wire; clients key by name, never by
    position. Bolt is the surface that fixes an entry order — decision 11's
    explicit re-keying to `code`, `title`, `description`, `severity`,
    `category`. `description` carries the same appended remedy as Bolt, and
    the `notifications` key is omitted when the list is empty. `notifications`
    is already named in the published v2 contract (`docs/api/http/spec.md`),
    so this fills the producer slot rather than inventing an extension key.

13. **Write-clause scope: unknown-name evidence comes from reads only.** The
    three `Unknown*Warning` kinds are derived from **read** patterns and
    **read** property accesses. A read pattern — a `MATCH` pattern, or a
    pattern nested in a pattern predicate, pattern comprehension or
    `shortestPath` — contributes its labels, relationship types and pattern
    property keys; a read expression property access (`n.key`,
    `expr { .key }`) contributes its key. A `CREATE` or
    `MERGE` target pattern contributes **nothing**: its labels, relationship
    types and pattern properties are names the statement itself brings into
    existence, so a name absent before the statement is not unknown, it is
    the write's own output. A `SET`/`REMOVE` property target
    (`SET n.key = …`, `REMOVE n.key`) likewise contributes nothing for that
    target key, though a property **read** on the right-hand side
    (`SET n.key = n.other`) still does. In a mixed statement the read part
    still warns: `MATCH (n:Persn) SET n.name = 'Ada'` reports 01N50 for
    `Persn` and nothing for `name`; `MERGE (p:Persn {name: 'Ada'})` reports
    nothing, because the merge would create `Persn` and `name`. The remedy
    wording follows from the scope: a warning is only ever raised for a name
    the statement reads that has never existed, so every `Unknown*Warning`
    keeps its spelling remedy — "Check the spelling; the label exists after
    the first write that creates it…", with the type and key equivalents —
    and that text is never attached to a write's own target. MCP's
    `write-cypher` runs the classification `EXPLAIN` from decision 10 on the
    write statement itself; under this rule that `EXPLAIN` no longer reports
    the labels, types or keys the write creates, so an agent's first
    `CREATE (:Person {name: 'Ada'})` on a fresh store returns no 01N50 or
    01N52 for `Person` or `name`, while a genuine typo in a read clause of
    the same write still reports.

## Alternatives considered

- Notifications on every run — rejected: deriving the list on the plain
  execution path forces the catalog/property-key walks and the list
  allocation into the hot path for every statement, contradicting ADR-0344's
  opt-in budget and the decision that plain runs do no notification work.
- An extra `remedy` key on Bolt and HTTP — rejected: drivers and Browser
  render the reference key set; `remedy` is neither Neo4j's nor part of the
  Query API v2 contract, and an unknown key invites silent drops or client
  warnings. Folding keeps one compatible shape and still carries the action;
  the engine-native form keeps the typed field.
- Threading source positions now — rejected: the front-end retains no source
  spans in its AST/IR, so this would add span fields to every node and thread
  them through the planner before a single notification could carry one; the
  notification model does not need positions to be useful. Deferred to a
  later ADR.
- Bolt 5.6 `statuses` — rejected: the server negotiates 4.4 only, a 5.6
  transport is its own project, and `statuses` would be a second channel
  duplicating the same facts for drivers that cannot negotiate it.
- Mapping every diagnostic onto a Neo4j code — rejected: `NoApplicableIndex`
  (03N93) and `EagerOperator` (03N94) are LOAD-CSV-specific triggers; using
  them for a missing index or an eager aggregation would misstate the
  diagnostic and hide the engine's own remedies.
- Putting the list on the plan tree only — rejected: non-operator
  notifications have no node to attach to, and every surface would
  re-derive the same vocabulary and text; one flat list beside the tree is
  the transport unit.
- Replacing the plan table's warnings with the notification list — rejected:
  it breaks the accepted ADR-0344/0345 plan presentation and its pinned
  tests, and loses the per-operator association in the table; the by-kind
  partition keeps both.
- Treating every pattern and every property access as unknown-name evidence
  (`CREATE`/`MERGE` target patterns and `SET`/`REMOVE` targets included) —
  rejected: those names are what the statement itself creates or removes, so
  a fresh store makes `EXPLAIN CREATE (:Person {name: 'Ada'})` warn that
  `Person` and `name` do not exist and advise "Check the spelling" about the
  statement's own output. Neo4j's `UnknownLabelWarning` and
  `UnknownPropertyKeyWarning` describe reads, and MCP `write-cypher` runs
  exactly this classification `EXPLAIN`, so every agent's first write would
  open with two false warnings; decision 13 scopes the evidence to reads.
- A statement-level read-only gate (derive unknown-name notifications only
  when the statement has no write clause) — rejected: it hides real typos in
  the read half of a write — `MATCH (n:Persn) SET n.name = 'Ada'` would lose
  the 01N50 — because scope is a property of a clause, not of a statement.
  The per-clause rule keeps the warning wherever a name is read and silences
  it only where the statement writes that name.

## Consequences

- Drivers, Browser and agents receive `notifications` in the shape they
  already render; unknown `CypherLite.*` codes render as generic
  information/warning entries rather than errors.
- The remedy is machine-usable on engine-native surfaces (`remedy`, e.g. the
  exact `CREATE INDEX`) and human-readable on Bolt/HTTP (appended to
  `description`). An agent reading MCP gets the statement it can run.
- Plain runs are unchanged: no tree, no list, no catalog walk. MCP gains
  notifications from work it already does, not from a second `EXPLAIN`.
- No position means an editor cannot jump to the offending source text, but
  the notification still names the label/type/key/function/procedure. A later
  ADR can add positions once the front-end retains spans.
- Adding a kind is a vocabulary addition: a true Neo4j analog adopts its
  code, an engine-own kind mints the next `03Cxx`/`01Cxx`, and table, module,
  docs and tests change together; a new namespace or category is a new ADR.
- The operator-scoped kinds that are not plan warnings
  (`UnboundedVariableLengthPattern`, `MissingIndex`, `EagerAggregation`)
  surface only in the notification block; the plan table's `Warnings` column
  keeps its three kinds, so the per-operator table and the transport list
  have different memberships by design.
- `cypherlite-model::wire::response_envelope`'s signature grows the list
  argument; the engine binding, MCP and the tests update in the same change,
  and Python's `QueryResult` pyclass gains the attribute; the surface specs
  and the CHANGELOG carry the new field or keys in that change.

## Later references

- ADR-0374: decision 7's two sibling fields (`plan` plus
  `notifications: Vec<Notification>`) become one `QueryResult.plan:
  Option<Box<PlanArtifacts>>` artifact carrying `root` and `notifications`,
  read through `plan_root()`/`notifications()`; the derivation, ordering,
  dedup and transport decisions above are unchanged.
- ADR-0535: the vocabulary gains the `HINT` category (ranked after
  `SCHEMA` in the decision 6 order) with the two planner-hint kinds
  `HintedIndexNotFound` (`01N31`) and `JoinHintUnfulfillable` (`01N30`),
  per this ADR's own "a new namespace or category is a new ADR"
  consequence.
