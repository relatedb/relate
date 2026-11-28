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
use relate_ir::{expr_vars, SearchFilterPred, SearchKind, SearchOp};
use relate_parser::Expr;

use super::expr::{cmp_op, flatten_and, unary_op};
use super::lower_expr;
use crate::ast::tree::{unparen, var_property};

/// Decompose and lower a `SEARCH` clause's inner `WHERE <search condition>`
/// into its predicate list against the clause's `binding_var` and
/// the index source's `FOR <expr>`. Which grammar runs is the `kind`'s
/// (ADR-0714): a `VECTOR` window takes the vector predicate forms —
/// flattened top-level `AND` (the one canonical [`flatten_and`]), the
/// `NOT <property>` and flipped-operand rewrites (the short-form-range
/// rewrite, e.g. `a < x.prop <= b`, needs no extra work here -- the
/// parser's own chained-comparison desugaring, `Parser::parse_comparison`,
/// already turns it into an `AND` of two ordinary comparisons before this
/// ever runs), the binding-variable-only rule, the per-property
/// AND-combination limits, the self-reference check, and the ban on `<>`;
/// a `FULLTEXT` window takes the match-filter family only — the
/// entity-set form and its id-spelled twin, at most once.
pub(super) fn lower_search_filters(
    binding_var: &SmallName,
    kind: SearchKind,
    for_expr: &Expr,
    where_expr: Option<&Expr>,
) -> Result<Vec<SearchFilterPred>> {
    let Some(where_expr) = where_expr else {
        // No inner WHERE: an empty predicate list (empty = no filters).
        return Ok(Vec::new());
    };
    // Each kind's grammar is stated once, at the dispatch (ADR-0714):
    // the vector window's predicate forms with their combination
    // rules, beside the fulltext window's match-filter-only grammar with
    // its at-most-one rule. The term walk is shared below; each grammar
    // closes over exactly its own context.
    Ok(match kind {
        SearchKind::Vector => {
            let for_var = for_entity_var(for_expr);
            lower_terms(
                where_expr,
                |term| to_predicate(binding_var, for_var, term),
                validate_and_combination,
            )?
        }
        SearchKind::Fulltext => lower_terms(
            where_expr,
            |term| lower_match_filter_term(binding_var, term),
            validate_match_filter_combination,
        )?,
    })
}

/// The shared term walk, the scaffold stated once: flatten the one
/// canonical `AND` ([`flatten_and`]), lower each term through the kind's
/// per-term lowering — a closure over the grammar's own context — then
/// the kind's combination rule, in that order. [`to_predicate`] with the
/// vector window's `FOR` entity variable and [`validate_and_combination`]
/// serve the vector forms; [`lower_match_filter_term`] and
/// [`validate_match_filter_combination`] the fulltext window's
/// match-filter grammar (ADR-0714).
fn lower_terms(
    where_expr: &Expr,
    lower_term: impl Fn(&Expr) -> Result<SearchFilterPred>,
    validate: fn(&[SearchFilterPred]) -> Result<()>,
) -> Result<Vec<SearchFilterPred>> {
    let mut terms = Vec::new();
    flatten_and(where_expr, &mut terms);
    let mut predicates = Vec::with_capacity(terms.len());
    for term in terms {
        predicates.push(lower_term(term)?);
    }
    validate(&predicates)?;
    Ok(predicates)
}

/// ADR-0714's fulltext per-term policy: the match filter only — the
/// entity-set form and its id-spelled twin. The term's `IN` node and LHS
/// classification are the gate's own output, so the term is classified
/// once and the lowering reads what the gate computed. Every other term
/// is the reference server's invalid-predicate rejection, raised before the vector
/// forms' own classification runs: the match-filter forms keep their
/// more specific rejections (a wrong variable on a legal form is the
/// entity-set rule's error, never the invalid-predicate one). This
/// grammar has no property predicates, so no `FOR` entity variable
/// feeds it.
fn lower_match_filter_term(binding_var: &SmallName, term: &Expr) -> Result<SearchFilterPred> {
    match classify_match_filter(term) {
        Some((in_expr, lhs)) => lower_in_predicate(binding_var, None, in_expr, lhs),
        None => Err(invalid_fulltext_predicate_error(term)),
    }
}

/// ADR-0714's fulltext gate: the term's `IN` node with its LHS
/// classification, when the term is one of the two match-filter forms —
/// the entity-set form ([`InLhs::EntityVar`]) or its id-spelled twin
/// ([`InLhs::IdCall`]); `None` for every other term. Terms arrive
/// unparenthesized ([`flatten_and`]'s contract), so the one
/// classification walk answers for the whole term.
fn classify_match_filter(term: &Expr) -> Option<(&relate_parser::InExpr, InLhs<'_>)> {
    match term {
        Expr::In(in_expr) => match classify_in_lhs(&in_expr.expr) {
            lhs @ (InLhs::IdCall(..) | InLhs::EntityVar(..)) => Some((in_expr, lhs)),
            InLhs::Property(..) | InLhs::Other => None,
        },
        _ => None,
    }
}

/// The entity-set match filter's at-most-one budget, stated once: the
/// count both windows' combination rules read — the classification lives
/// in the predicate's own parts projection
/// ([`SearchFilterPred::is_entity_filter`]). The vector window's
/// per-property group rules live in [`validate_and_combination`] beside
/// it; the fulltext window has no property predicates, so the budget is
/// its whole combination rule.
fn entity_filter_count(predicates: &[SearchFilterPred]) -> usize {
    predicates
        .iter()
        .filter(|predicate| predicate.is_entity_filter())
        .count()
}

/// The fulltext combination rule: the match filter appears at most once
/// (the budget in the reference server's invalid-predicate family — the sentence
/// never names the wrong index kind).
fn validate_match_filter_combination(predicates: &[SearchFilterPred]) -> Result<()> {
    if entity_filter_count(predicates) > 1 {
        return Err(invalid_fulltext_family_error("combination"));
    }
    Ok(())
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

/// ADR-0714's fulltext error family, one constructor (the vector
/// family's `invalid_predicate_error` shape): the reference server's
/// invalid-predicate frame — `SyntaxError` class — with the rejection's
/// own detail interpolated. The term form renders the predicate's
/// spelling so the rejection names what the user wrote; the combination
/// form names the combination rule.
fn invalid_fulltext_family_error(detail: &str) -> Error {
    Error::syntax(
        EngineError::semantic(
            ErrorKind::SyntaxError,
            format!(
                "invalid predicate for search with filters. The search filter \
                 predicate {detail} is not supported, see documentation for \
                 all supported predicates."
            ),
        )
        .to_string(),
    )
}

/// The term form: the predicate's own spelling — the one structural
/// renderer ([`crate::ast::structural_expr_text`]) with this
/// sentence's placeholder for a shape outside the grammar — in the
/// family frame. The sentence is advisory — its GQLSTATUS class carries
/// the contract — so faithfulness to the common shapes is the bar, never
/// a parse.
fn invalid_fulltext_predicate_error(term: &Expr) -> Error {
    invalid_fulltext_family_error(&format!(
        "'{}'",
        crate::ast::structural_expr_text(term, "<predicate>")
    ))
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

/// The vector window's per-term lowering: one arm per accepted
/// `VectorFilterExpression` form, the binding-variable rule stated once
/// per LHS kind before the single predicate construction. Predicate
/// shape: a property predicate's LHS names the clause's binding variable,
/// or the index source's own entity variable (`FOR m.embedding` ->
/// `m.rating`) with the binding variable on the RHS. An entity-set
/// predicate (`<variable> IN <expr>`) must name the binding variable
/// itself.
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
        // The `IN` predicate lowers from its LHS classification (see
        // [`InLhs`]) — the one home of the three accepted forms'
        // construction, shared with the fulltext gate (which carries the
        // classification it gated on, so a term is classified once).
        Expr::In(i) => lower_in_predicate(binding_var, for_var, i, classify_in_lhs(&i.expr)),
        _ => Err(invalid_predicate_error()),
    }
}

/// The `IN` predicate's lowering from its LHS classification (see
/// [`InLhs`]): `id(<binding variable>)` is the entity-set match filter's
/// id-spelled twin (ADR-0678; the right-hand side may be a correlated
/// `COLLECT` subquery); a bare variable is the entity-set match filter;
/// a variable property is the property-in-set filter. Only the binding
/// variable's own id is the candidate's id — any other variable is the
/// wrong-variable rejection (the entity-set rule's, no FOR-entity
/// exception either). The `id` name reads through the registry's own
/// lowercase-then-compare idiom (`validate/index_hint.rs`'s spelling),
/// not a second ad-hoc function-name discrimination.
fn lower_in_predicate(
    binding_var: &SmallName,
    for_var: Option<&SmallName>,
    in_expr: &relate_parser::InExpr,
    lhs: InLhs<'_>,
) -> Result<SearchFilterPred> {
    match lhs {
        InLhs::IdCall(call, var) => {
            let rhs = lower_expr(&in_expr.list)?;
            validate_property_rule(var, binding_var, None, Some(&rhs))?;
            Ok(SearchFilterPred::IdInSet {
                call: call.clone(),
                rhs,
            })
        }
        InLhs::EntityVar(var) => {
            let rhs = lower_expr(&in_expr.list)?;
            // The entity-set LHS deliberately gets no FOR-entity
            // exception: the match filter is over the binding variable.
            validate_property_rule(var, binding_var, None, Some(&rhs))?;
            Ok(SearchFilterPred::EntityInSet { rhs })
        }
        InLhs::Property(var, prop) => {
            let rhs = lower_expr(&in_expr.list)?;
            validate_property_rule(var, binding_var, for_var, Some(&rhs))?;
            Ok(SearchFilterPred::InSet {
                property: prop.to_string(),
                rhs,
            })
        }
        InLhs::Other => Err(invalid_predicate_error()),
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
        total: usize,
        lower: usize,
        upper: usize,
    }
    let entity_filters = entity_filter_count(predicates);
    let mut groups: IdentHashMap<&str, CombinationCounts> = IdentHashMap::default();
    for predicate in predicates {
        // Only a predicate that reads an indexed property forms a group
        // — the entity-set match filters carry none (they match on
        // candidate identity; the parts projection states it), so their
        // whole combination rule is the budget above.
        let Some(property) = predicate.property_name() else {
            continue;
        };
        let entry = groups.entry(property).or_default();
        entry.total += 1;
        // A comparison's direction counts toward the bounded-range
        // shape; the existence forms and the property-in-set form are
        // plain group members.
        if let SearchFilterPred::Comparison { op, .. } = predicate {
            match op {
                SearchOp::Gt | SearchOp::Gte => entry.lower += 1,
                SearchOp::Lt | SearchOp::Lte => entry.upper += 1,
                SearchOp::Eq => {}
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

/// The search-filter grammar's test bench, split into its sibling file
/// (the `pathdb_tests` precedent): the file stays the production grammar
/// module, the bench keeps its own file.
#[cfg(test)]
mod search_filter_tests;
