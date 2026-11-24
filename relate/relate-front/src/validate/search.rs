//! The `<search clause>` rule family: the SEARCH-clause
//! binding-variable, SCORE-conflict, and scope rules of
//! [`validate_search_clauses`].

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, SearchClause, SearchKind};

use crate::ast::collect::{bind_pattern_vars, collect_expr_variables, search_score_var};
use crate::ast::search_spec::{walk_search_spec, SearchSlot};

/// `<search clause>` rules that don't fit any existing generic
/// scope tracker:
/// - The search query's self-reference ban (the search query can't
///   reference the binding variable it's about to produce).
/// - The binding variable reference must be declared by this same
///   MATCH statement's own pattern (its NODE/RELATIONSHIP value-type
///   half is checked at runtime, see `match_clause::execute_search_for_row`).
/// - The SCORE AS variable must not conflict with anything already
///   in scope, including the binding variable reference itself.
///
/// The pattern-shape restriction this battery once carried (one bound
/// variable, a single node or single-hop relationship, predicates only on
/// the binding) is gone: a searching MATCH keeps every pattern shape the
/// general MATCH machinery accepts (ADR-0678), so the shape family has no
/// validation rule here — the corpus that pinned the rejections now
/// expects their results.
///
/// The SCORE-conflict scope check builds `scope` by walking the body's clauses from
/// empty, deriving each clause's introduced names from
/// [`clause_introduced_names`]: a SEARCH clause sees the names earlier
/// clauses of the same linear body declared. (A CIP-213 NEXT stage starts a
/// fresh body -- its names are bound per stage on the lowered IR, not by
/// this pass.)
pub(crate) fn validate_search_clauses(clauses: &[Clause]) -> Result<(), EngineError> {
    let mut scope: IdentHashSet<SmallName> = IdentHashSet::default();

    for clause in clauses {
        match clause {
            // A searching MATCH is the one arm this fold spells itself: the
            // SEARCH rules need the clause's own pattern variables *before*
            // they enter the scope (the binding-reference and
            // SCORE-conflict checks both test against them), while
            // [`clause_introduced_names`] folds the pattern variables and the
            // SCORE variable together.
            Clause::Match(mc) => match mc.search.as_ref() {
                Some(search) => {
                    let mut this_match_vars: IdentHashSet<SmallName> = IdentHashSet::default();
                    bind_pattern_vars(&mc.pattern, &mut this_match_vars);
                    validate_search_clause_rules(search, &this_match_vars, &scope)?;
                    scope.extend(this_match_vars);
                    // The SCORE conflict, checked where the scope lifecycle lives: after
                    // this MATCH's own variables have entered the scope, one
                    // `insert` decides the conflict (and binds on win).
                    if let Some(sv) = search_score_var(mc) {
                        if !scope.insert(sv.clone()) {
                            return Err(EngineError::semantic(
                                ErrorKind::VariableAlreadyDefined,
                                format!("variable '{sv}' is already defined"),
                            ));
                        }
                    }
                }
                None => crate::call_scope::advance_scope(&mut scope, clause),
            },
            clause => crate::call_scope::advance_scope(&mut scope, clause),
        }
    }
    Ok(())
}

/// The SEARCH rules proper, extracted from the MATCH arm's scope
/// bookkeeping: the binding-variable reference, and the
/// source-specification rules -- the per-index kind×option compatibility
/// (on the index) and the self-reference rules (the `FOR` query and the
/// WEIGHT/CONSTANT controls, by slot) -- as one slot-tagged pass over the
/// (possibly nested, CIP-278) source tree, the first violation in walk
/// order stopping it. The SCORE-conflict check stays with the
/// caller, which owns the scope lifecycle.
fn validate_search_clause_rules(
    search: &SearchClause,
    this_match_vars: &IdentHashSet<SmallName>,
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    if !this_match_vars.contains(&search.binding_var) {
        return Err(EngineError::semantic(
            ErrorKind::InvalidSearchVariableReference,
            format!(
                "'{}' must be a variable bound by this SEARCH clause's own MATCH statement",
                search.binding_var
            ),
        ));
    }

    walk_search_spec(
        &search.spec,
        // The kind×option compatibility policy, one spelling per kind and
        // one statement of its polarity: `WITH ANALYZER` and SKIP/OFFSET
        // are fulltext-only, and SKIP/OFFSET is the vector side's
        // rejection (CIP-225 grammar -- accepting SKIP on a VECTOR INDEX
        // silently deviates from the surface, round-4 review); the inner
        // WHERE serves both kinds (ADR-0678 -- the reference corpus runs
        // match-filter predicates on fulltext sources). The rejections
        // raise the typed `InvalidUseOfCommand` detail name.
        &mut |index| {
            match index.kind {
                SearchKind::Vector => {
                    if index.analyzer.is_some() {
                        return Err(EngineError::semantic(
                            ErrorKind::InvalidUseOfCommand,
                            "WITH ANALYZER cannot be used together with VECTOR INDEX".to_string(),
                        ));
                    }
                    if index.offset_expr.is_some() {
                        return Err(EngineError::semantic(
                            ErrorKind::InvalidUseOfCommand,
                            "SKIP cannot be used together with VECTOR INDEX".to_string(),
                        ));
                    }
                }
                SearchKind::Fulltext => {}
            }
            Ok(())
        },
        // The self-reference rules, dispatched by slot: the search
        // query (`FOR`), the WEIGHT/CONSTANT controls and the
        // per-row `WITH ANALYZER` expression may reference any incoming
        // variable but not the search clause's own binding variable
        // (FulltextSearchAcceptance [59]/[60]). The inner WHERE is
        // deliberately excluded -- its predicate shape is `binding
        // variable . property` (or the binding variable itself), so it
        // names the binding variable by design.
        &mut |slot, expr| {
            // One collection of the expression's variables serves every
            // slot's policy; one match over the slot vocabulary decides
            // it — the window slots delegate to their own named rule,
            // the binding-reference slots yield their message and share
            // the trailing check.
            let mut vars = IdentHashSet::default();
            collect_expr_variables(expr, &mut vars);
            let message = match slot {
                SearchSlot::InnerWhere => return Ok(()),
                // The LIMIT and SKIP expressions read the input row, never
                // a pattern binding: the executor states one window for a
                // whole round, so a per-row window is rejected under every
                // shape, not silently collapsed to the first row's.
                SearchSlot::Limit => {
                    return check_window_slot("LIMIT", &vars, this_match_vars, scope)
                }
                SearchSlot::Offset => {
                    return check_window_slot("SKIP", &vars, this_match_vars, scope)
                }
                SearchSlot::For => "the search query cannot reference the binding variable",
                SearchSlot::Weight | SearchSlot::Constant => {
                    "the WEIGHT or CONSTANT expression may not depend on the search clause \
                     binding variable"
                }
                SearchSlot::Analyzer => {
                    "the analyzer expression may not depend on the search clause binding variable"
                }
            };
            if vars.contains(&search.binding_var) {
                return Err(EngineError::semantic(
                    ErrorKind::SelfReferencingSearch,
                    format!("{message} '{}'", search.binding_var),
                ));
            }
            Ok(())
        },
    )
}

/// The window rule (ADR-0678's split): a LIMIT or SKIP expression reads
/// the input row, never a variable this MATCH's pattern newly binds —
/// the executor states one window for a whole round, so a per-row window
/// is rejected under every shape, not silently collapsed to the first
/// row's. The reported name is the deterministic minimum of the reads
/// (IdentHashSet iteration is process-random, ADR-0505 §4); the clause
/// word comes with the caller.
fn check_window_slot(
    window: &str,
    vars: &IdentHashSet<SmallName>,
    this_match_vars: &IdentHashSet<SmallName>,
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    let newly_bound = |var: &SmallName| this_match_vars.contains(var) && !scope.contains(var);
    if let Some(var) = vars.iter().filter(|v| newly_bound(v)).min() {
        return Err(EngineError::semantic(
            ErrorKind::SyntaxError,
            format!(
                "SearchClauseWindowReadsPattern: the {window} expression \
                 may not depend on the pattern variable '{var}'"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window expression (LIMIT or SKIP) reading a pattern variable is
    /// rejected: the executor states one window per round, so a per-row
    /// window is refused at validation under every shape.
    #[test]
    fn a_window_reading_a_pattern_variable_is_rejected() {
        // Live Neo4j 2026.10.1 rejects the same LIMIT reads with 42N28
        // ("only statically inferrable patterns and variables are allowed
        // in 'LIMIT'"); the SKIP slot carries the same rule — a per-row
        // SKIP would page each row's window differently.
        for query in [
            "MATCH (m:Movie) \
             SEARCH m IN (FULLTEXT INDEX ft FOR 'x' LIMIT toInteger(m.keep)) \
             RETURN m",
            "MATCH (m:Movie)-[]->() \
             SEARCH m IN (FULLTEXT INDEX ft FOR 'x' LIMIT toInteger(m.keep)) \
             RETURN m",
            "MATCH (m:Movie) \
             SEARCH m IN (FULLTEXT INDEX ft FOR 'x' SKIP toInteger(m.keep) LIMIT 1) \
             RETURN m",
        ] {
            let error =
                crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
                    .unwrap_err();
            assert!(
                error.to_string().contains("SearchClauseWindowReadsPattern"),
                "{query}: {error}"
            );
        }
    }

    /// ADR-0678: the pattern-shape family no longer rejects — the corpus
    /// that pinned the rejections now expects their results, so the shapes
    /// parse and validate (their execution is pinned executor-side and in
    /// `relate/relate/tests/`).
    #[test]
    fn complex_search_match_patterns_validate() {
        for query in [
            "MATCH (seed:Person {name: 'Alice'}), (p:Person) \
             SEARCH p IN (FULLTEXT INDEX ft FOR 'brave' LIMIT 5) RETURN p",
            "MATCH (p:Person)-[:KNOWS*1..2]->(target:Person) \
             SEARCH p IN (FULLTEXT INDEX ft FOR 'brave' LIMIT 5) RETURN p",
            "MATCH (m:Movie)-[r]->() \
             SEARCH m IN (FULLTEXT INDEX ft FOR 'brave' LIMIT 5) RETURN m",
            "MATCH (m:Movie)-[]->(:Book) \
             SEARCH m IN (FULLTEXT INDEX ft FOR 'brave' LIMIT 5) RETURN m",
            "MATCH (movie:Movie), () \
             SEARCH movie IN (VECTOR INDEX v FOR [1.0] LIMIT 5) RETURN movie",
        ] {
            crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
                .unwrap_or_else(|error| panic!("{query}: {error}"));
        }
    }

    /// The CIP-224 rules that raise typed [`ErrorKind`]s render with their
    /// own detail name (the payload unchanged after it) -- pinned here
    /// because the rendered text is the error contract, raised by the
    /// validate battery before lowering.
    #[test]
    fn search_rules_raise_their_typed_kinds() {
        for (query, kind, text) in [
            (
                "MATCH (n) SEARCH m IN (FULLTEXT INDEX ft FOR 'x' LIMIT 1) RETURN n",
                ErrorKind::InvalidSearchVariableReference,
                "InvalidSearchVariableReference: 'm' must be a variable bound by this \
                 SEARCH clause's own MATCH statement",
            ),
            (
                "MATCH (n) SEARCH n IN (FULLTEXT INDEX ft FOR n.title LIMIT 1) RETURN n",
                ErrorKind::SelfReferencingSearch,
                "SelfReferencingSearch: the search query cannot reference the binding \
                 variable 'n'",
            ),
            (
                "MATCH (n) SEARCH n IN (FULLTEXT INDEX ft FOR 'x' LIMIT 1) SCORE AS n \
                 RETURN n",
                ErrorKind::VariableAlreadyDefined,
                "VariableAlreadyDefined: variable 'n' is already defined",
            ),
            (
                "MATCH (n) SEARCH n IN (VECTOR INDEX v FOR [1.0] WITH ANALYZER 'a' LIMIT 1) \
                 RETURN n",
                ErrorKind::InvalidUseOfCommand,
                "InvalidUseOfCommand: WITH ANALYZER cannot be used together with VECTOR INDEX",
            ),
            (
                "MATCH (n) SEARCH n IN (VECTOR INDEX v FOR [1.0] SKIP 1 LIMIT 1) RETURN n",
                ErrorKind::InvalidUseOfCommand,
                "InvalidUseOfCommand: SKIP cannot be used together with VECTOR INDEX",
            ),
        ] {
            let error =
                crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
                    .unwrap_err();
            assert_eq!(error.kind(), kind, "wrong kind for: {query}");
            assert_eq!(error.to_string(), text, "wrong text for: {query}");
        }
    }

    /// ADR-0678: the inner WHERE serves FULLTEXT sources — the old
    /// WHERE-with-FULLTEXT rejection is gone (the reference corpus runs
    /// match-filter predicates on fulltext indexes), and the remaining
    /// kind×option rejections keep firing.
    #[test]
    fn the_inner_where_serves_both_index_kinds() {
        crate::validate::validate_statement_tree(
            &relate_parser::parse(
                "MATCH (n) SEARCH n IN (FULLTEXT INDEX ft FOR 'x' WHERE n.p > 1 LIMIT 1) \
                 RETURN n",
            )
            .unwrap(),
        )
        .expect("the fulltext inner WHERE validates");
    }
}
