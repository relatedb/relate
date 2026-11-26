//! Compat callables (ADR-0699): a front-end table of foreign procedure
//! names re-implemented as Cypher bodies.
//!
//! A remote client written against Neo4j calls procedure names Relate has
//! no engine code for — `neo4j-graphrag`'s writer, for one, stops on
//! `ProcedureNotFound` for four names whose behavior the dialect already
//! expresses (dynamic property keys, dynamic labels, dynamic relationship
//! types). This module answers such names without engine code: when a
//! parsed statement calls a table entry, the entry's body — ordinary
//! reviewable Cypher, parsed once from its `DEFINE PROCEDURE` spelling —
//! is injected into the statement's definitions and the local-callable resolver
//! ([`crate::local_callables`], ADR-0382) rewrites every call site away. The
//! planner and executor never see the foreign name.
//!
//! The injection runs in the one resolve-and-lower tail every parse entry
//! shares ([`crate::parse_and_lower_statement_with_policy_and_params`]
//! threads the store predicate into it), so no entry can omit the step.
//!
//! Precedence (ADR-0699): a user `DEFINE` of the same name **and kind**
//! wins (the injection skips a name the statement defines anywhere in its
//! tree, matched in the definition's own namespace — a user FUNCTION
//! carrying a table procedure's name does not suppress that procedure,
//! ADR-0382); a
//! store-registered procedure wins (the engine's admission passes
//! `Graph::has_procedure`, which answers whether the store has the name —
//! `true` declines the table entry); only then is the table entry
//! injected. The table is the last registry consulted, never the first.
//!
//! An entry can be a deliberate no-op — a body of `FINISH` makes the call
//! succeed and do nothing — the form for foreign procedures whose
//! behavior Relate consciously declines but whose name a client needs to
//! call without error. Distinct from the executor's catalog-only rows
//! (listed, call fails); a table entry is callable.
//!
//! Nothing user-controlled ever reaches the text path: the table is
//! static maintainer-authored text, and call arguments are bound as
//! *variables* by the call-by-value rewrite, never spliced into the body
//! — so there is no injection surface to sanitize. Body templating is
//! deliberately absent (ADR-0699, deferred).

use std::cell::Cell;
use std::sync::OnceLock;

use relate_core::Result;
use relate_parser::{Clause, Expr, LocalCallableKind, LocalDefinition, Statement};

use crate::ast::tree::{walk_statement_tree, walker_error};

/// One parameter of a compat callable: its name, declared type (the
/// executor signature vocabulary — `NODE`, `STRING`, `LIST<FLOAT>`, …),
/// and its `= <literal>` default when the parameter is optional.
#[derive(Debug)]
pub struct CompatParam {
    /// The parameter name — the body references it under exactly this
    /// name.
    pub name: &'static str,
    /// The declared argument type, shown by `SHOW PROCEDURES` and checked
    /// by compile-time `CALL` validation.
    pub ty: &'static str,
    /// The written default (`"{}"`, `"null"`, …); `None` for a mandatory
    /// parameter.
    pub default: Option<&'static str>,
}

impl CompatParam {
    /// A mandatory parameter.
    pub(crate) const fn new(name: &'static str, ty: &'static str) -> Self {
        Self {
            name,
            ty,
            default: None,
        }
    }

    /// An optional parameter with a written default.
    pub(crate) const fn with_default(
        name: &'static str,
        ty: &'static str,
        default: &'static str,
    ) -> Self {
        Self {
            name,
            ty,
            default: Some(default),
        }
    }
}

/// One compat-callables table row: the name, parameters, yield columns and
/// body the injection reads, plus the catalog description the executor's
/// `BUILTIN_PROCEDURE_SIGNATURES` row repeats (pinned to this table by the
/// drift-guard test there). Engine classification — `writes`, `executable`,
/// `listed` — lives in the signature table alone; this table states only
/// what the front end owns: membership, the parameter contract, and the
/// body (ADR-0699).
#[derive(Debug)]
pub struct CompatCallable {
    /// The fully-qualified foreign name as called, verbatim.
    pub name: &'static str,
    /// `Procedure` or `Function` — the two namespaces are independent
    /// (ADR-0382).
    pub kind: LocalCallableKind,
    /// Parameters in declaration order; the body references each by name.
    pub params: &'static [CompatParam],
    /// Yield columns in order, as the body's terminal RETURN emits them
    /// (pinned by the health test); empty for a void procedure.
    pub returns: &'static [&'static str],
    /// The catalog description, naming the Cypher re-implementation.
    pub description: &'static str,
    /// The body as Cypher text — the clauses between the definition's
    /// braces. `FINISH` alone is the no-op form.
    pub body: &'static str,
}

/// The compat-callables table (ADR-0699). The seed set answers
/// `neo4j-graphrag`'s writer (relatedb/relate#862); each new entry is a
/// dialect decision that names the client contract it answers.
pub static COMPAT_CALLABLES: &[CompatCallable] = &[
    CompatCallable {
        name: "db.create.setNodeVectorProperty",
        kind: LocalCallableKind::Procedure,
        params: &[
            CompatParam::new("node", "NODE"),
            CompatParam::new("key", "STRING"),
            CompatParam::new("vector", "LIST<FLOAT>"),
        ],
        returns: &[],
        description: "Compatibility (ADR-0699): stores the embedding as a \
                      list property — the Cypher `SET node[key] = vector` \
                      the dynamic property key expresses.",
        body: "SET node[key] = vector FINISH",
    },
    CompatCallable {
        name: "db.create.setRelationshipVectorProperty",
        kind: LocalCallableKind::Procedure,
        params: &[
            CompatParam::new("relationship", "RELATIONSHIP"),
            CompatParam::new("key", "STRING"),
            CompatParam::new("vector", "LIST<FLOAT>"),
        ],
        returns: &[],
        description: "Compatibility (ADR-0699): stores the embedding as a \
                      list property — the Cypher `SET relationship[key] = \
                      vector` the dynamic property key expresses.",
        body: "SET relationship[key] = vector FINISH",
    },
    CompatCallable {
        name: "apoc.create.addLabels",
        kind: LocalCallableKind::Procedure,
        params: &[
            CompatParam::new("nodes", "NODE"),
            CompatParam::new("labels", "LIST<STRING>"),
        ],
        returns: &["node"],
        description: "Compatibility (ADR-0699): adds the labels through \
                      Cypher's dynamic labels — `SET nodes:$(labels)`.",
        body: "SET nodes:$(labels) RETURN nodes AS node",
    },
    CompatCallable {
        name: "apoc.merge.relationship",
        kind: LocalCallableKind::Procedure,
        params: &[
            CompatParam::new("start", "NODE"),
            CompatParam::new("relType", "STRING"),
            CompatParam::new("identProps", "MAP"),
            CompatParam::new("onCreateProps", "MAP"),
            CompatParam::new("end", "NODE"),
            CompatParam::with_default("onMatchProps", "MAP", "{}"),
        ],
        returns: &["rel"],
        description: "Compatibility (ADR-0699): merges through Cypher's \
                      dynamic relationship types — `MERGE \
                      (start)-[r:$(relType)]->(end) ON CREATE SET r += \
                      … ON MATCH SET r += …`. `identProps` is accepted and \
                      ignored (callers pass `{}`).",
        body: "MERGE (start)-[r:$(relType)]->(end) \
               ON CREATE SET r += coalesce(onCreateProps, {}) \
               ON MATCH SET r += coalesce(onMatchProps, {}) \
               RETURN r AS rel",
    },
];

/// The `DEFINE` text of one entry — its name, parameters and body as one
/// reviewable spelling. Only the table's own `&'static` fields are
/// interpolated here; nothing user-controlled reaches this text (the
/// call-by-value rewrite binds arguments as variables, never splices
/// them into the body).
fn define_text(entry: &CompatCallable) -> String {
    let kind = match entry.kind {
        LocalCallableKind::Procedure => "PROCEDURE",
        LocalCallableKind::Function => "FUNCTION",
    };
    let params = entry
        .params
        .iter()
        .map(|param| match param.default {
            Some(default) => format!("{} = {}", param.name, default),
            None => param.name.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "DEFINE {kind} {}({params}) {{ {} }}",
        entry.name, entry.body
    )
}

/// The table's definitions, parsed once and shared: every injected call
/// site clones its own copy of the body (the same contract the resolver
/// applies to user definitions). A table row whose `DEFINE` text does not
/// parse is a table bug, not a query error — the parse is pinned by test
/// and fails loudly here rather than degrading any query.
fn table_definitions() -> &'static [LocalDefinition] {
    static DEFINITIONS: OnceLock<Vec<LocalDefinition>> = OnceLock::new();
    DEFINITIONS.get_or_init(|| {
        COMPAT_CALLABLES
            .iter()
            .map(|entry| {
                let mut stmt = relate_parser::parse(&define_text(entry))
                    .expect("the compat-callables table's DEFINE text parses (pinned by test)");
                stmt.definitions.pop().expect(
                    "the compat-callables table's DEFINE text carries one definition (pinned by test)",
                )
            })
            .collect()
    })
}

/// The mask width of the injection's table-index bits: a `u64` holds the
/// first 64 table entries. The bound is asserted at every mask read
/// ([`index_bit`]) and pinned by the table's health test, so a 65th entry
/// fails loudly instead of silently aliasing bit 0.
const MAX_TABLE_ENTRIES: usize = 64;

/// The index's mask bit — the one shift, width-asserted at every read
/// (the table's length is not const-readable, so each read carries the
/// loud bound; a release build cannot alias a bit past the width).
fn index_bit(index: usize) -> u64 {
    assert!(
        index < MAX_TABLE_ENTRIES,
        "the compat-callables table exceeds the u64 mask width; widen the mask type"
    );
    1u64 << index
}

/// The one membership scan: the name's position in the table, `None` for
/// a name outside it. Every membership question reads this — the kind
/// filter ([`table_bit`]), the executor's dispatch backstop
/// ([`is_compat_callable`]) and the admission's snapshot predicate
/// ([`registered_in`]) — never a second name scan.
fn table_position(name: &str) -> Option<usize> {
    COMPAT_CALLABLES.iter().position(|entry| entry.name == name)
}

/// The name's table bit, `0` unless the entry at the name's position is
/// of that kind — procedures and functions are independent namespaces
/// (ADR-0382), so a kind mismatch is "not a member". Read by the
/// injection walk.
fn table_bit(name: &str, kind: LocalCallableKind) -> u64 {
    table_position(name)
        .filter(|&index| COMPAT_CALLABLES[index].kind == kind)
        .map_or(0, index_bit)
}

/// Whether `name` is a compat-callables table entry (of either kind) —
/// read by the executor's dispatch backstop.
pub fn is_compat_callable(name: &str) -> bool {
    table_position(name).is_some()
}

/// The store-predicate snapshot half of the admission's ADR-0699 guard:
/// probe every table entry once, under the caller's short-lived
/// publication read guard, so the guard never has to span the parse
/// itself. [`registered_in`] answers from the snapshot.
pub fn registration_snapshot(store_has_procedure: &dyn Fn(&str) -> bool) -> u64 {
    COMPAT_CALLABLES
        .iter()
        .enumerate()
        .filter(|(_, entry)| store_has_procedure(entry.name))
        .map(|(index, _)| index_bit(index))
        .fold(0u64, |mask, bit| mask | bit)
}

/// The predicate half of [`registration_snapshot`]: whether the store had
/// the name registered when the snapshot was taken — the membership
/// lookup stays in this module, so the caller never re-scans the table by
/// name. A registration racing the snapshot takes effect at the next
/// statement's snapshot.
pub fn registered_in(name: &str, snapshot: u64) -> bool {
    table_position(name).is_some_and(|index| snapshot & index_bit(index) != 0)
}

/// OR a visited name's table bit (if any, of the visited kind) into one
/// of the walk's masks — the one spelling of the walk's per-name action,
/// shared by the statement, clause and expression visitors. Kind-scoped
/// (ADR-0382): a user `DEFINE FUNCTION` carrying a table procedure's name
/// sets no procedure bit, so it cannot suppress that procedure's
/// injection — the two namespaces are independent.
fn or_bit(mask: &Cell<u64>, name: &str, kind: LocalCallableKind) {
    let bit = table_bit(name, kind);
    if bit != 0 {
        mask.set(mask.get() | bit);
    }
}

/// Inject the table entries a statement calls, in table order, onto its
/// top-level definitions (ADR-0699). Definitions head the statement, so
/// an injected definition is visible at every call site of its block and
/// every block nested within it. Skipped for a name the statement itself
/// defines anywhere in its tree **in the table entry's own kind** (a user
/// `DEFINE` of the same name and kind wins; the namespaces are independent,
/// ADR-0382) or the store registers (`store_has_procedure` answers whether the store has the
/// name — `true` declines the table entry). A no-op when the statement
/// calls no table name.
///
/// The two facts — which entries the statement calls, and which it
/// defines itself — are collected as one bitmask per fact inside a single
/// walk: each visited name is matched against the small table by string
/// comparison, so no owned `String` is cloned and no hash set is built
/// for statements that call no procedure at all.
pub(crate) fn inject(
    stmt: &mut Statement,
    store_has_procedure: &dyn Fn(&str) -> bool,
) -> Result<()> {
    // The three visitor closures run simultaneously, so both masks sit
    // behind a `Cell` (the same interior-mutability shape `body_weight`'s
    // counter uses).
    let called = Cell::new(0u64);
    let defined = Cell::new(0u64);
    walk_statement_tree(
        stmt,
        &mut |node| {
            for definition in node.definitions() {
                definition
                    .name
                    .with_str(|name| or_bit(&defined, name, definition.kind));
            }
            Ok(())
        },
        &mut |clause| {
            if let Clause::CallProcedure(call) = clause {
                call.procedure
                    .with_str(|name| or_bit(&called, name, LocalCallableKind::Procedure));
            }
            Ok(())
        },
        &mut |expr| {
            if let Expr::FunctionCall(call) = expr {
                call.name
                    .with_str(|name| or_bit(&called, name, LocalCallableKind::Function));
            }
            Ok(())
        },
    )
    .map_err(walker_error)?;
    let called = called.get();
    if called == 0 {
        // No table name is called: nothing to inject, and the lazy
        // table-definition parse stays untouched.
        return Ok(());
    }
    let defined = defined.get();
    // Prepend, in table order, ahead of any user definition: the resolver
    // registers a block's definitions in list order and resolves each
    // body at registration, so a compat name called from inside a user
    // `DEFINE` body must find the injected definition already registered.
    let mut injected = Vec::new();
    for (index, (entry, definition)) in COMPAT_CALLABLES.iter().zip(table_definitions()).enumerate()
    {
        let bit = index_bit(index);
        if called & bit == 0 || defined & bit != 0 || store_has_procedure(entry.name) {
            continue;
        }
        injected.push(definition.clone());
    }
    stmt.definitions.splice(0..0, injected);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Statement {
        relate_parser::parse(src).expect("test statement parses")
    }

    /// The statement's definition names, in registration order — the one
    /// projection the injection tests assert on.
    fn definition_names(stmt: &Statement) -> Vec<String> {
        stmt.definitions
            .iter()
            .map(|definition| definition.name.to_string())
            .collect()
    }

    /// The table's own health: every entry's DEFINE text parses into
    /// exactly one definition whose name, kind, parameter names,
    /// optional-default presence and emitted yield columns match the row —
    /// the executor's signature rows and the injected bodies both read
    /// this one table, so a row that does not round-trip is a table bug
    /// caught here. The yield-column check reads the same derivation the
    /// resolver's YIELD mapping uses, so a body edited away from
    /// `entry.returns` fails here before `SHOW PROCEDURES` can advertise
    /// a column the rewritten body does not emit.
    #[test]
    fn every_table_entry_parses_into_one_matching_definition() {
        // The mask bound first: a table past it would alias bits before
        // any row's health is even read.
        assert!(
            COMPAT_CALLABLES.len() <= MAX_TABLE_ENTRIES,
            "the injection's u64 masks hold {MAX_TABLE_ENTRIES} entries; widen the mask type"
        );
        for (entry, definition) in COMPAT_CALLABLES.iter().zip(table_definitions()) {
            assert_eq!(definition.name.to_string(), entry.name, "{entry:?}");
            assert_eq!(definition.kind, entry.kind, "{entry:?}");
            assert_eq!(definition.params.len(), entry.params.len(), "{entry:?}");
            for (param, spec) in definition.params.iter().zip(entry.params) {
                assert_eq!(param.name.to_string(), spec.name, "{entry:?}");
                assert_eq!(
                    param.default.is_some(),
                    spec.default.is_some(),
                    "the default's presence must match the table row: {entry:?}"
                );
            }
            let relate_parser::LocalDefinitionBody::Query(body) = &definition.body else {
                panic!("a compat procedure body is a braced query: {entry:?}")
            };
            let emitted = crate::local_callables::body_export_names(body);
            match emitted {
                Some(names) => {
                    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
                    assert_eq!(names, entry.returns, "the body's RETURN must emit the row's yield columns: {entry:?}");
                }
                None => assert!(
                    entry.returns.is_empty(),
                    "a body with no terminal RETURN is void; the row must declare no yield columns: {entry:?}"
                ),
            }
        }
    }

    #[test]
    fn a_called_table_name_is_injected() {
        let mut stmt = parse(
            "CREATE (n:N) WITH n, [\"X\"] AS ls \
             CALL apoc.create.addLabels(n, ls) YIELD node RETURN labels(node) AS l",
        );
        inject(&mut stmt, &|_| false).unwrap();
        assert_eq!(definition_names(&stmt), ["apoc.create.addLabels"]);
    }

    #[test]
    fn a_statement_without_table_names_is_untouched() {
        let mut stmt = parse("CREATE (n:N {a: 1}) RETURN n.a");
        inject(&mut stmt, &|_| false).unwrap();
        assert!(stmt.definitions.is_empty());
    }

    /// User `DEFINE` wins (ADR-0699 precedence): a name the statement
    /// defines anywhere — nested bodies included — is never injected, so
    /// the resolver's unit-wide name registry never sees a duplicate.
    #[test]
    fn a_user_defined_name_is_not_injected() {
        let mut stmt = parse(
            "DEFINE PROCEDURE apoc.create.addLabels(nodes, labels) { \
             SET nodes:$(labels) RETURN nodes AS node } \
             CREATE (n:N) WITH n, [\"X\"] AS ls \
             CALL apoc.create.addLabels(n, ls) YIELD node RETURN labels(node) AS l",
        );
        inject(&mut stmt, &|_| false).unwrap();
        assert_eq!(stmt.definitions.len(), 1);
        assert_eq!(
            stmt.definitions[0].name.to_string(),
            "apoc.create.addLabels"
        );
    }

    /// A user definition nested in a subquery body is still a user
    /// definition — the injection must skip the name there too.
    #[test]
    fn a_user_defined_name_in_a_nested_body_is_not_injected() {
        let mut stmt = parse(
            "CREATE (n:N) WITH n, [\"X\"] AS ls CALL (n, ls) { \
             DEFINE PROCEDURE apoc.create.addLabels(nodes, labels) { \
             SET nodes:$(labels) RETURN nodes AS node } \
             CALL apoc.create.addLabels(n, ls) YIELD node RETURN labels(node) AS l }",
        );
        inject(&mut stmt, &|_| false).unwrap();
        assert!(
            stmt.definitions.is_empty(),
            "the nested definition owns the name; nothing is injected at the top level"
        );
    }

    /// Kind scoping (ADR-0382): a user `DEFINE FUNCTION` carrying a table
    /// procedure's name is a different namespace — it must not suppress
    /// the procedure's injection (the kind-agnostic mask would have left
    /// the call unrewritten, and a void entry would then have resolved as
    /// a silent no-op).
    #[test]
    fn a_user_function_of_a_table_procedure_name_does_not_suppress_the_injection() {
        let mut stmt = parse(
            "DEFINE FUNCTION apoc.create.addLabels(nodes, labels) = labels \
             CREATE (n:N) WITH n, [\"X\"] AS ls \
             CALL apoc.create.addLabels(n, ls) YIELD node RETURN labels(node) AS l",
        );
        inject(&mut stmt, &|_| false).unwrap();
        // The user function plus the injected procedure, in registration
        // order — the same name twice, so the kinds carry the distinction:
        // the injected PROCEDURE first (the resolver registers in list
        // order), the user's FUNCTION second.
        assert_eq!(
            definition_names(&stmt),
            ["apoc.create.addLabels", "apoc.create.addLabels"]
        );
        assert_eq!(stmt.definitions[0].kind, LocalCallableKind::Procedure);
        assert_eq!(stmt.definitions[1].kind, LocalCallableKind::Function);
        // The injected definition precedes the user's (the resolver
        // registers in list order), so a compat call inside a user DEFINE
        // body finds it already registered.
        assert_eq!(
            stmt.definitions[0].name.to_string(),
            "apoc.create.addLabels"
        );
        assert_eq!(stmt.definitions[0].kind, LocalCallableKind::Procedure);
        assert_eq!(stmt.definitions[1].kind, LocalCallableKind::Function);
    }

    /// A compat call inside a user DEFINE body: the injected definition
    /// registers before the user's body resolves, so the call rewrites
    /// through it (the appended-after order once left it unrewritten, and
    /// the executor backstop answered it as a bypassed rewrite).
    #[test]
    fn a_compat_call_inside_a_user_define_body_is_rewritten() {
        let mut stmt = parse(
            "DEFINE PROCEDURE relabel(n, l) { \
             CALL apoc.create.addLabels(n, l) YIELD node RETURN node } \
             CREATE (m:M) WITH m, [\"X\"] AS ls CALL relabel(m, ls) YIELD node \
             RETURN labels(node) AS l",
        );
        inject(&mut stmt, &|_| false).unwrap();
        assert_eq!(
            definition_names(&stmt),
            ["apoc.create.addLabels", "relabel"]
        );
    }

    /// The store predicate wins (ADR-0699 precedence): a store-registered
    /// procedure of the same name is not shadowed by the table entry.
    #[test]
    fn a_store_registered_name_is_not_injected() {
        let mut stmt = parse(
            "CREATE (n:N) WITH n, [\"X\"] AS ls \
             CALL apoc.create.addLabels(n, ls) YIELD node RETURN labels(node) AS l",
        );
        inject(&mut stmt, &|name| name == "apoc.create.addLabels").unwrap();
        assert!(stmt.definitions.is_empty());
    }

    /// The engine admission path end to end at the front: a compat call
    /// lowers through the gate-then-inject entry with no preview opt-in —
    /// an injected definition is engine-provided, so it never trips the
    /// `local-callables` preview gate. Routing through the gated entry
    /// (not the ungated convenience) pins both facts: the injection ran,
    /// and the gate did not fire.
    #[test]
    fn a_compat_call_lowers_without_the_local_callables_preview() {
        crate::parse_and_lower_statement_with_preview(
            "CREATE (n:N) WITH n, [\"X\"] AS ls \
             CALL apoc.create.addLabels(n, ls) YIELD node RETURN labels(node) AS l",
            relate_core::PreviewFeatures::none(),
        )
        .unwrap();
        crate::parse_and_lower_statement_with_preview(
            "CREATE (a:A), (b:B) \
             CALL apoc.merge.relationship(a, \"REL\", {}, {w: 1}, b, {w: 2}) YIELD rel \
             RETURN type(rel) AS t",
            relate_core::PreviewFeatures::none(),
        )
        .unwrap();
        crate::parse_and_lower_statement_with_preview(
            "CREATE (n:N) WITH n CALL db.create.setNodeVectorProperty(n, 'e', [0.1, 0.2]) \
             RETURN n.e AS e",
            relate_core::PreviewFeatures::none(),
        )
        .unwrap();
    }
}
