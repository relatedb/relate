//! CIP-240 "Vector Search with Single-stage Filtering": the one grammar
//! walk over a `SEARCH` clause's inner `WHERE <search condition>`, run at
//! this module's lowering boundary, emitting the IR `SearchFilterPred`
//! list directly (the predicate-form, binding-variable, per-property,
//! self-reference and simple-operator rules enforced here; the RHS
//! lowered, the `NOT <property>` rewrite's FALSE literal supplied). The
//! index-property-membership check needs the resolved index and is
//! checked at execution, matching how the NODE/RELATIONSHIP entity-type
//! check already works for the outer CIP-224 `SEARCH` surface.

use crate::ident_hash::IdentHashMap;
use relate_names::SmallName;

use relate_core::{Error, Result};
use relate_error::{EngineError, ErrorKind};
use relate_ir::{expr_vars, SearchFilterPred, SearchOp};
use relate_parser::Expr;

use super::expr::{cmp_op, flatten_and, unary_op};
use super::lower_expr;
use crate::ast::tree::{unparen, var_property};

/// Decompose and lower a `SEARCH` clause's inner `WHERE <search condition>`
/// into its predicate list against the clause's `binding_var` and
/// the index source's `FOR <expr>`: flattens top-level `AND` (the one
/// canonical [`flatten_and`]), applies the `NOT <property>` and
/// flipped-operand rewrites (the short-form-range rewrite, e.g.
/// `a < x.prop <= b`, needs no extra work here -- the parser's own
/// chained-comparison desugaring, `Parser::parse_comparison`, already
/// turns it into an `AND` of two ordinary comparisons before this ever
/// runs), then enforces the binding-variable-only rule, the per-property
/// AND-combination limits, the self-reference check, and the ban on `<>`.
pub(super) fn lower_search_filters(
    binding_var: &SmallName,
    for_expr: &Expr,
    where_expr: Option<&Expr>,
) -> Result<Vec<SearchFilterPred>> {
    let Some(where_expr) = where_expr else {
        // No inner WHERE: an empty predicate list (empty = no filters).
        return Ok(Vec::new());
    };
    // Predicate shape: a property predicate's LHS names the clause's binding
    // variable, or the index source's own entity variable (`FOR m.embedding`
    // -> `m.rating`) with the binding variable on the RHS. An entity-set
    // predicate (`<variable> IN <expr>`) must name the binding variable
    // itself.
    let for_var = for_entity_var(for_expr);
    let mut terms = Vec::new();
    flatten_and(where_expr, &mut terms);
    let mut predicates = Vec::with_capacity(terms.len());
    for term in terms {
        predicates.push(to_predicate(binding_var, for_var, term)?);
    }
    validate_and_combination(&predicates)?;
    Ok(predicates)
}
/// The index source's entity variable, when `FOR <expr>` names an entity
/// property (`FOR m.embedding`); a literal or any other query expression
/// (`FOR [1, 2, 3]`) names no entity variable.
fn for_entity_var(for_expr: &Expr) -> Option<&SmallName> {
    var_property(for_expr).map(|(var, _)| var)
}

/// The variable an `id(<variable>)` argument names, when the argument is
/// exactly a bare variable (paren-transparent): `id(r)` is `r`, anything
/// else (`id(r.prop)`, `id(id(r))`, a literal) is not the id-spelled
/// match filter's LHS.
fn id_argument_variable(arg: &Expr) -> Option<&SmallName> {
    match unparen(arg) {
        Expr::Variable(var) => Some(var),
        _ => None,
    }
}

/// The `IN` predicate's accepted LHS forms — one classification walk,
/// three shapes (see [`classify_in_lhs`]).
enum InLhs<'a> {
    /// `id(<variable>)`, the id-spelled entity-set match filter: the
    /// call's own name beside the variable it wraps.
    IdCall(&'a SmallName, &'a SmallName),
    /// A bare variable: the entity-set match filter.
    EntityVar(&'a SmallName),
    /// A variable property: the property-in-set filter.
    Property(&'a SmallName, &'a SmallName),
    /// No accepted form.
    Other,
}

/// Classify an `IN` predicate's LHS with one paren-transparent walk: the
/// `id(<one argument>)` call (the `id` name through the registry's
/// lowercase-then-compare idiom, `validate/index_hint.rs`'s spelling)
/// whose argument is a bare variable, a bare variable itself, or a
/// variable property. Anything else is [`InLhs::Other`]. The id-call
/// form carries the call's own name — the one site that recognizes it
/// is also the one site that spells it.
fn classify_in_lhs(lhs: &Expr) -> InLhs<'_> {
    let lhs = unparen(lhs);
    if let Expr::FunctionCall(call) = lhs {
        if call.name.with_str(str::to_ascii_lowercase) == "id" && call.args.len() == 1 {
            return match id_argument_variable(&call.args[0]) {
                Some(var) => InLhs::IdCall(&call.name, var),
                None => InLhs::Other,
            };
        }
    }
    match lhs {
        Expr::Variable(var) => InLhs::EntityVar(var),
        other => match var_property(other) {
            Some((var, prop)) => InLhs::Property(var, prop),
            None => InLhs::Other,
        },
    }
}

/// The same error the pre-lowering validation phase raised for this
/// check, through the front's established bridge into the lowering's
/// core `Error` (kind and text preserved).
fn invalid_predicate_error() -> Error {
    Error::syntax(
        EngineError::semantic(
            ErrorKind::SyntaxError,
            "InvalidSearchFilterPredicate: invalid predicate for vector search filtering. \
             A vector search filter must consist of one or more predicates joined by AND, and the \
             combined predicates for each property must specify either an exact value (e.g. `x.prop = \
             1`), a half-bounded range (e.g. `x.prop >= 1`), or a bounded range (e.g. `x.prop > 1 AND \
             x.prop < 100`)",
        )
        .to_string(),
    )
}

/// The wrong-variable violation (Neo4j's 42I74): the predicate names a variable
/// other than the binding variable (see [`validate_property_rule`]).
fn wrong_variable_error() -> Error {
    Error::syntax("WrongVariableForVectorSearchFilter")
}

/// Whether the lowered expression references `name` (the one
/// `expr_vars` walk).
fn references_var(expr: &relate_ir::Expr, name: &SmallName) -> bool {
    // The one std-hashed, String-keyed set left in the lowering path:
    // `expr_vars` is `relate-ir`'s API and its `&mut HashSet<String>`
    // parameter fixes both key and hasher here (ADR-0509 stage 2 re-keys
    // the front's own scope maps; the ir walkers migrate on their own
    // node). Membership only — the set is never iterated.
    let mut variables = std::collections::HashSet::new();
    expr_vars(expr, &mut variables);
    // Probed allocation-free through the stack text read.
    name.with_str(|text| variables.contains(text))
}

/// CIP-240 Rules 6 then 1, in that order (Neo4j raises 42I75 before
/// 42I74): the RHS may not reference the predicate's own LHS variable, and
/// the LHS property must be the binding variable's -- or, when it names
/// the index source's entity variable (`FOR m.embedding` -> `m.rating`),
/// the RHS must reference the binding variable, so the predicate still
/// identifies the searched entity. A predicate with no RHS (the NOT
/// rewrite, the existence forms) cannot take the entity-variable
/// exception.
fn validate_property_rule(
    var: &SmallName,
    binding_var: &SmallName,
    for_var: Option<&SmallName>,
    rhs: Option<&relate_ir::Expr>,
) -> Result<()> {
    if let Some(rhs) = rhs {
        if references_var(rhs, var) {
            return Err(Error::syntax("SelfReferencingVectorSearchFilter"));
        }
    }
    let rhs_references_binding = rhs.is_some_and(|rhs| references_var(rhs, binding_var));
    if var == binding_var || (for_var == Some(var) && rhs_references_binding) {
        Ok(())
    } else {
        Err(wrong_variable_error())
    }
}

fn to_predicate(
    binding_var: &SmallName,
    for_var: Option<&SmallName>,
    term: &Expr,
) -> Result<SearchFilterPred> {
    // Each arm recognizes one accepted `VectorFilterExpression` form; the
    // binding-variable rule is then stated once per LHS kind before the
    // single predicate construction.
    match term {
        // The `NOT <property>` rewrite: `NOT <property>` ->
        // `<property> = FALSE`. Anything else under a `NOT` (an equality,
        // a range, an arbitrary expression) isn't rewritable and is
        // rejected.
        Expr::Unary(u) if matches!(unary_op(u.op), Ok(relate_ir::UnaryOp::Not)) => {
            let Some((var, prop)) = var_property(&u.expr) else {
                return Err(invalid_predicate_error());
            };
            // The NOT rewrite carries no user RHS, so it cannot identify
            // the binding variable through one.
            validate_property_rule(var, binding_var, for_var, None)?;
            // The NOT rewrite's RHS is the FALSE literal.
            Ok(SearchFilterPred::Comparison {
                property: prop.to_string(),
                op: SearchOp::Eq,
                rhs: relate_ir::Expr::Bool(false),
            })
        }
        Expr::Comparison(c) => {
            // Only a `<simple comp op>` serves (`<>` and `=~` are
            // typed ops the canonical lowering recognizes, declined here
            // at the one construction site).
            let Some(op) =
                SearchOp::from_binary(cmp_op(c.op).map_err(|_| invalid_predicate_error())?)
            else {
                return Err(invalid_predicate_error());
            };
            // The flipped-operand rewrite: the predicate's candidate
            // side is the searched entity's own property, so a side naming
            // the binding variable wins -- the independent-property
            // form `q.rating <= p.rating` filters the candidates on
            // `p.rating >= q.rating`, with `q.rating` the per-row RHS (live
            // Neo4j 2026.10.1 returns the same rows either way round).
            // Otherwise the canonical left property reference wins and the
            // right side is an ordinary RHS expression -- the
            // self-reference check below then catches it if it depends on
            // the LHS variable (e.g. `movie.year = movie.rating`).
            let left_property = var_property(&c.left);
            let right_property = var_property(&c.right);
            // One guarded step picks the candidate side — the binding
            // variable's own property is the LHS — then one arm per
            // shape: both sides are properties with the binding on the
            // right (the left is the RHS), any left-property shape (the
            // right is the RHS), left-is-not-a-property (the right is
            // the LHS), neither (not a property predicate).
            let (var, prop, op, rhs_expr) = match (left_property, right_property) {
                (Some(l), Some(r)) if r.0 == binding_var && l.0 != binding_var => {
                    (r.0, r.1, op.flip(), &c.left)
                }
                (Some((var, prop)), _) => (var, prop, op, &c.right),
                (None, Some((var, prop))) => (var, prop, op.flip(), &c.left),
                (None, None) => return Err(invalid_predicate_error()),
            };
            let rhs = lower_expr(rhs_expr)?;
            validate_property_rule(var, binding_var, for_var, Some(&rhs))?;
            Ok(SearchFilterPred::Comparison {
                property: prop.to_string(),
                op,
                rhs,
            })
        }
        // `<property> IS [NOT] NULL`: existence on the indexed entity's
        // additional property.
        Expr::IsNull(i) => {
            let Some((var, prop)) = var_property(&i.expr) else {
                return Err(invalid_predicate_error());
            };
            validate_property_rule(var, binding_var, for_var, None)?;
            Ok(if i.negated {
                SearchFilterPred::Exists {
                    property: prop.to_string(),
                }
            } else {
                SearchFilterPred::NotExists {
                    property: prop.to_string(),
                }
            })
        }
        // The `IN` predicate's LHS, classified once into its three
        // accepted forms (see [`InLhs`]): `id(<binding variable>)` is the
        // entity-set match filter's id-spelled twin (ADR-0678; the
        // right-hand side may be a correlated `COLLECT` subquery); a
        // bare variable is the entity-set match filter; a variable
        // property is the property-in-set filter. Only the binding
        // variable's own id is the candidate's id — any other variable is
        // the wrong-variable rejection (the entity-set rule's, no
        // FOR-entity exception either). The `id` name reads through the
        // registry's own lowercase-then-compare idiom
        // (`validate/index_hint.rs`'s spelling), not a second ad-hoc
        // function-name discrimination.
        Expr::In(i) => match classify_in_lhs(&i.expr) {
            InLhs::IdCall(call, var) => {
                let rhs = lower_expr(&i.list)?;
                validate_property_rule(var, binding_var, None, Some(&rhs))?;
                Ok(SearchFilterPred::IdInSet {
                    call: call.clone(),
                    rhs,
                })
            }
            InLhs::EntityVar(var) => {
                let rhs = lower_expr(&i.list)?;
                // The entity-set LHS deliberately gets no FOR-entity
                // exception: the match filter is over the binding
                // variable.
                validate_property_rule(var, binding_var, None, Some(&rhs))?;
                Ok(SearchFilterPred::EntityInSet { rhs })
            }
            InLhs::Property(var, prop) => {
                let rhs = lower_expr(&i.list)?;
                validate_property_rule(var, binding_var, for_var, Some(&rhs))?;
                Ok(SearchFilterPred::InSet {
                    property: prop.to_string(),
                    rhs,
                })
            }
            InLhs::Other => Err(invalid_predicate_error()),
        },
        _ => Err(invalid_predicate_error()),
    }
}

/// One property admits a single predicate, or exactly one
/// lower-bound and one upper-bound range forming a bounded range. Anything
/// else -- two lower bounds, two equalities, an equality beside a range --
/// is rejected (Neo4j's `queryExpressionForOneProperty`). The
/// "contradictory predicates return no results" note still
/// holds: contradictory *bounds* are a legal two-predicate range.
/// The entity-set match filter appears at most once (Neo4j's
/// `queryExpressionFromWhereClause`).
fn validate_and_combination(predicates: &[SearchFilterPred]) -> Result<()> {
    /// One property group's predicate-kind counts.
    #[derive(Default)]
    struct CombinationCounts {
        total: u32,
        lower: u32,
        upper: u32,
    }
    let mut entity_filters = 0u32;
    let mut groups: IdentHashMap<&str, CombinationCounts> = IdentHashMap::default();
    for predicate in predicates {
        match predicate {
            SearchFilterPred::EntityInSet { .. } | SearchFilterPred::IdInSet { .. } => {
                entity_filters += 1
            }
            SearchFilterPred::Comparison { property, op, .. } => {
                let entry = groups.entry(property.as_str()).or_default();
                entry.total += 1;
                match op {
                    SearchOp::Gt | SearchOp::Gte => entry.lower += 1,
                    SearchOp::Lt | SearchOp::Lte => entry.upper += 1,
                    SearchOp::Eq => {}
                }
            }
            SearchFilterPred::Exists { property }
            | SearchFilterPred::NotExists { property }
            | SearchFilterPred::InSet { property, .. } => {
                groups.entry(property.as_str()).or_default().total += 1;
            }
        }
    }
    if entity_filters > 1 {
        return Err(invalid_predicate_error());
    }
    for counts in groups.values() {
        let bounded_range = counts.total == 2 && counts.lower == 1 && counts.upper == 1;
        if counts.total > 1 && !bounded_range {
            return Err(invalid_predicate_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lower the filters of the statement's one SEARCH clause directly, so
    /// the accepted/rejected grammar shapes are pinned without a catalog.
    fn lower_filters(query: &str) -> Result<Vec<SearchFilterPred>> {
        let stmt = relate_parser::parse(query).expect("parses");
        let search = stmt
            .clauses
            .iter()
            .find_map(|clause| match clause {
                relate_parser::Clause::Match(clause) => clause.search.as_ref(),
                _ => None,
            })
            .expect("expected a SEARCH subclause");
        let relate_parser::SearchSpec::Index(spec) = &search.spec else {
            panic!("expected a single index source")
        };
        lower_search_filters(
            &search.binding_var,
            &spec.for_expr,
            spec.inner_where.as_ref(),
        )
    }

    fn expect_error(query: &str, expected: &str) {
        let error = lower_filters(query).expect_err("the predicate must be rejected");
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?} in {error}"
        );
    }

    #[test]
    fn property_predicates_may_name_the_for_entity_variable() {
        // `FOR m.embedding` names the index entity; the binding variable's
        // side is the candidate property whatever the operand order, so
        // `m.rating <= movie.rating` filters on `movie.rating >= m.rating`
        // with the row's `m.rating` as the RHS (live Neo4j 2026.10.1
        // returns the same rows as the `movie.rating >= m.rating` form).
        let filters = lower_filters(
            "MATCH (m:Movie) MATCH (movie:Movie) \
             SEARCH movie IN (VECTOR INDEX moviePlots FOR m.embedding \
             WHERE m.rating <= movie.rating LIMIT 2) RETURN movie.title",
        )
        .expect("accepted");
        let [SearchFilterPred::Comparison {
            property,
            op: SearchOp::Gte,
            rhs,
        }] = filters.as_slice()
        else {
            panic!("expected one comparison on the binding variable: {filters:?}")
        };
        assert_eq!(property, "rating");
        assert!(
            matches!(rhs, relate_ir::Expr::Property { .. }),
            "the row's `m.rating` is the RHS: {rhs:?}"
        );
    }

    #[test]
    fn existence_predicates_are_accepted() {
        let filters = lower_filters(
            "MATCH (movie:Movie) \
             SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
             WHERE movie.rating IS NOT NULL AND movie.year IS NULL LIMIT 2) \
             RETURN movie.title",
        )
        .expect("accepted");
        assert!(matches!(
            filters.as_slice(),
            [
                SearchFilterPred::Exists { property: exists },
                SearchFilterPred::NotExists { property: not_exists },
            ] if exists == "rating" && not_exists == "year"
        ));
    }

    #[test]
    fn in_predicates_cover_property_and_entity_sets() {
        let filters = lower_filters(
            "MATCH (movie:Movie) \
             SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
             WHERE movie.rating IN [7.6, 7.7] AND movie IN godFathers LIMIT 2) \
             RETURN movie.title",
        )
        .expect("accepted");
        assert!(matches!(
            filters.as_slice(),
            [
                SearchFilterPred::InSet { property, .. },
                SearchFilterPred::EntityInSet { .. },
            ] if property == "rating"
        ));
    }

    #[test]
    fn for_entity_lhs_without_a_binding_variable_rhs_is_rejected() {
        // The FOR entity variable on the LHS is only the accepted
        // "independent property as LHS" form when the RHS names the
        // binding variable; a constant RHS is the wrong-variable error.
        expect_error(
            "MATCH (m:Movie) MATCH (movie:Movie) \
             SEARCH movie IN (VECTOR INDEX moviePlots FOR m.embedding \
             WHERE m.rating > 7 LIMIT 5) RETURN movie.title",
            "WrongVariableForVectorSearchFilter",
        );
    }

    #[test]
    fn property_predicate_on_an_unrelated_variable_is_rejected() {
        // Without a FOR entity variable only the binding variable is
        // accepted (Neo4j 42I74).
        expect_error(
            "MATCH (m:Movie) MATCH (movie:Movie) \
             SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
             WHERE m.rating > 7 LIMIT 5) RETURN movie.title",
            "WrongVariableForVectorSearchFilter",
        );
    }

    #[test]
    fn entity_set_predicate_must_name_the_binding_variable() {
        // Even the FOR entity variable is not an entity-set LHS: the filter
        // would apply to the wrong symbol (Neo4j 42I74).
        expect_error(
            "MATCH ()-[r {name: 'cs'}]->() MATCH ()-[rel]->() \
             SEARCH rel IN (VECTOR INDEX relIndex FOR r.vector \
             WHERE r IN [0] LIMIT 10) RETURN rel.id",
            "WrongVariableForVectorSearchFilter",
        );
    }

    #[test]
    fn rhs_may_not_reference_the_lhs_variable() {
        expect_error(
            "MATCH (movie:Movie) \
             SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
             WHERE movie.year = movie.rating LIMIT 2) RETURN movie.title",
            "SelfReferencingVectorSearchFilter",
        );
    }

    #[test]
    fn one_property_admits_a_single_predicate_or_one_bounded_range() {
        lower_filters(
            "MATCH (movie:Movie) \
             SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
             WHERE movie.year > 1945 AND movie.year < 2000 LIMIT 2) \
             RETURN movie.title",
        )
        .expect("a bounded range is two compatible predicates");
        for rejected in [
            "WHERE movie.year > 1945 AND movie.year > 2000",
            "WHERE movie.rating = 7.3 AND movie.rating = 7.4",
            "WHERE movie.rating = 7.3 AND movie.rating > 7",
            "WHERE movie.rating = 7.3 AND movie.year = 1999 AND movie.rating = 7.4",
        ] {
            expect_error(
                &format!(
                    "MATCH (movie:Movie) SEARCH movie IN (VECTOR INDEX moviePlots \
                     FOR [1, 2, 3] {rejected} LIMIT 2) RETURN movie.title"
                ),
                "InvalidSearchFilterPredicate",
            );
        }
    }

    #[test]
    fn more_than_one_entity_set_predicate_is_rejected() {
        expect_error(
            "MATCH (movie:Movie) \
             SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
             WHERE movie IN a AND movie IN b LIMIT 2) RETURN movie.title",
            "InvalidSearchFilterPredicate",
        );
    }

    /// ADR-0678: `id(<binding variable>) IN <expr>` is the entity-set
    /// match filter's id-spelled twin; any other variable inside the
    /// `id()` is the wrong-variable rejection, and the two membership
    /// spellings share the at-most-one entity-filter budget.
    #[test]
    fn id_membership_predicates_name_the_binding_variable() {
        let filters = lower_filters(
            "MATCH ()-[r {name: 'cs'}]->() MATCH ()-[rel]->() \
             SEARCH rel IN (VECTOR INDEX relIndex FOR r.vector \
             WHERE id(rel) IN [10, 40] LIMIT 10) RETURN rel.id",
        )
        .expect("accepted");
        assert!(
            matches!(filters.as_slice(), [SearchFilterPred::IdInSet { .. }]),
            "{filters:?}"
        );
        expect_error(
            "MATCH ()-[r {name: 'cs'}]->() MATCH ()-[rel]->() \
             SEARCH rel IN (VECTOR INDEX relIndex FOR r.vector \
             WHERE id(r) IN [10] LIMIT 10) RETURN rel.id",
            "WrongVariableForVectorSearchFilter",
        );
        expect_error(
            "MATCH (movie:Movie) \
             SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
             WHERE id(movie.rating) IN [1] LIMIT 2) RETURN movie.title",
            "InvalidSearchFilterPredicate",
        );
        expect_error(
            "MATCH (movie:Movie) \
             SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
             WHERE movie IN a AND id(movie) IN b LIMIT 2) RETURN movie.title",
            "InvalidSearchFilterPredicate",
        );
    }
}
