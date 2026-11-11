//! CREATE/MERGE write-pattern legality: exactly one relationship type,
//! directed relationships, no variable-length relationships or quantified
//! path patterns in writes, CIP-156 parameter property-map shorthand
//! (CREATE-only) and the CIP-156 dynamic label/type scope and quantifier
//! rules. The constant dynamic-operand check lives in [`super::dynamic`].

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use crate::ast::collect::{check_expr_vars_defined, has_relationship};
use crate::ast::tree::walk_label_expr;
use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, LabelExpr, PatternElement, PatternPath};

/// 42N59: declaring a name that is already bound is `VariableAlreadyBound`,
/// unless the enclosing scope admits shadowing it, or the use is a reference
/// to the existing entity. The one policy for the CREATE/MERGE pattern
/// binder, a `CALL … YIELD`/SHOW column binder and CIP-30's `REPORT
/// STATUS AS` (the last two pass `reference = false` and an empty shadow
/// exemption).
pub(crate) fn declare_new_name(
    name: &SmallName,
    bound: impl Fn(&SmallName) -> bool,
    shadowable: impl Fn(&SmallName) -> bool,
    reference: bool,
) -> Result<(), EngineError> {
    if bound(name) && !shadowable(name) && !reference {
        return Err(name.with_str(EngineError::duplicate_variable));
    }
    Ok(())
}

/// True if this relationship type spec is anything other than exactly one
/// plain type name -- used by the CREATE/MERGE "exactly one relationship
/// type" validation, which now must also reject GQL label-expression
/// operators (`|`, `&`, `!`, `%`), not just old-style multi-type pipe lists.
fn is_single_type(type_expr: &Option<LabelExpr>) -> bool {
    // A dynamic type atom's resolved count isn't known until runtime
    // (CIP-156) -- the exactly-one-type rule for CREATE/MERGE relationships
    // is enforced against the resolved name list instead (see
    // the executor's `eval::labels::resolve_write_labels`).
    if type_expr.as_ref().is_some_and(LabelExpr::contains_dynamic) {
        return true;
    }
    matches!(
        type_expr
            .as_ref()
            .and_then(|e| e.as_label_list())
            .as_deref(),
        Some([_])
    )
}
/// Validate CREATE patterns for VariableAlreadyBound errors.
/// In CREATE, a node variable that is already bound must not introduce new labels
/// or properties — it can only be used as a reference. If it has labels or props,
/// that's an error. If it has neither and is bound, it's also VariableAlreadyBound
/// (you can't CREATE a node that's already bound).
pub(crate) fn validate_create_bindings(
    patterns: &[PatternPath],
    bound_vars: &IdentHashSet<SmallName>,
    shadowable: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    use relate_parser::Direction;
    // Track variables defined within THIS CREATE clause (across multiple paths)
    let mut create_node_vars: IdentHashSet<SmallName> = IdentHashSet::default();

    for path in patterns {
        for elem in &path.elements {
            match elem {
                PatternElement::Node(np) => {
                    if let Some(var) = &np.variable {
                        let declared_here = create_node_vars.contains(var);
                        // Even an already-bound node is legal as a bare
                        // reference endpoint beside a relationship — unless it
                        // introduces labels/properties ("re-creating" it).
                        let reference =
                            has_relationship(path) && !super::self_reference::declares_shape(np);
                        declare_new_name(
                            var,
                            |name| bound_vars.contains(name) || create_node_vars.contains(name),
                            // A name the enclosing scope bound (and this clause
                            // did not itself declare earlier) is shadowable: the
                            // CREATE declares a fresh binding for it. Inside a
                            // FOREACH body that is the loop's enclosing names; at
                            // top level the set is empty, so this is a no-op.
                            |name| shadowable.contains(name) && !declared_here,
                            reference,
                        )?;
                        create_node_vars.insert(var.clone());
                    }
                }
                PatternElement::Relationship(rp) => {
                    // A relationship variable that is already bound (e.g. from a
                    // preceding MATCH — Create2 [23] `MATCH ()-[r]->() CREATE
                    // ()-[r]->()`) is `VariableAlreadyBound`. This check runs
                    // before the single-type/direction checks because the
                    // already-bound diagnosis is the more specific (and
                    // openCypher-expected) one — a bound `r` reused with no type
                    // must report VariableAlreadyBound, not NoSingleRelationshipType.
                    // An enclosing name stays shadowable, so a FOREACH body may
                    // re-declare it.
                    if let Some(var) = &rp.variable {
                        if bound_vars.contains(var) && !shadowable.contains(var) {
                            return Err(var.with_str(EngineError::duplicate_variable));
                        }
                    }
                    // CREATE relationships must have exactly one type
                    if !is_single_type(&rp.type_expr) {
                        return Err(EngineError::semantic(
                            ErrorKind::NoSingleRelationshipType,
                            "CREATE relationships must have exactly one type".to_string(),
                        ));
                    }
                    // CREATE relationships must have a direction (not undirected)
                    if rp.direction == Direction::Both {
                        return Err(EngineError::semantic(
                            ErrorKind::RequiresDirectedRelationship,
                            "CREATE relationships must have a direction".to_string(),
                        ));
                    }
                    // CREATE relationships cannot be variable-length
                    if rp.is_variable_length() {
                        return Err(EngineError::semantic(
                            ErrorKind::CreatingVarLength,
                            "variable length relationships cannot be used in CREATE".to_string(),
                        ));
                    }
                    // (The already-bound relationship-variable check now runs at
                    // the top of this arm, before the single-type check.)
                }
                PatternElement::Quantified(_) => {
                    return Err(EngineError::semantic(
                        ErrorKind::QuantifiedPathPatternNotSupported,
                        "quantified path patterns cannot be used in CREATE".to_string(),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The two write clauses whose patterns reject element predicates. The
/// variant is the single source of the diagnostic label and of whether the
/// element property values are scope-checked (CREATE's are, MERGE's are
/// not), so the two facts cannot drift apart.
#[derive(Clone, Copy)]
pub(crate) enum WriteClause {
    Create,
    Merge,
}

impl WriteClause {
    fn label(self) -> &'static str {
        match self {
            WriteClause::Create => "CREATE",
            WriteClause::Merge => "MERGE",
        }
    }

    fn checks_pattern_props(self) -> bool {
        matches!(self, WriteClause::Create)
    }
}

/// The CREATE/MERGE pattern-element walker: every element's pattern
/// predicate is rejected (the clause named in the message), a quantified
/// element is rejected outright, and the element's variable joins the
/// scope. The clause kind also decides whether the property values are
/// scope-checked. The scope-coupled CREATE/MERGE dispatch stays in
/// [`super::expressions`] (the DELETE precedent: the target admission
/// policy lives in its own concern module, the dispatch in `expressions`).
pub(crate) fn walk_write_elements(
    elements: &[PatternElement],
    clause: WriteClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for elem in elements {
        // One body over the shared element shape: the where-expression,
        // the property map and the variable are all a node and a
        // relationship element carry alike.
        let (where_expr, properties, variable) = match elem {
            PatternElement::Node(np) => {
                (np.where_expr.as_ref(), &np.properties, np.variable.as_ref())
            }
            PatternElement::Relationship(rp) => {
                (rp.where_expr.as_ref(), &rp.properties, rp.variable.as_ref())
            }
            PatternElement::Quantified(_) => {
                return Err(EngineError::semantic(
                    ErrorKind::QuantifiedPathPatternNotSupported,
                    format!(
                        "quantified path patterns cannot be used in {}",
                        clause.label()
                    ),
                ));
            }
        };
        // Element-pattern WHERE (CIP-17) is scoped to matching only;
        // Neo4j rejects it in a write clause.
        if where_expr.is_some() {
            return Err(EngineError::semantic(
                ErrorKind::InvalidNodePatternPredicate,
                format!(
                    "node and relationship pattern predicates cannot be used in a {} clause",
                    clause.label()
                ),
            ));
        }
        if clause.checks_pattern_props() {
            for (_key, val_expr) in properties {
                check_expr_vars_defined(val_expr, scope)?;
            }
        }
        if let Some(v) = variable {
            scope.insert(v.clone());
        }
    }
    Ok(())
}

/// CIP-156's `$paramName` whole-property-map pattern shorthand (e.g.
/// `CREATE (n:Label $props)`) is CREATE-only -- the parser accepts the
/// syntax generically for any pattern position, but Neo4j rejects it in
/// MATCH/MERGE patterns with `InvalidParameterUse` (TCK
/// `clauses/match/Match1.feature` scenario 6, `Match2.feature` scenario 8,
/// `clauses/merge/Merge1.feature` scenario 16).
///
/// Checks one linear body. Nested bodies (`CALL { }`, `WHEN`/`ELSE`, UNION
/// arms, NEXT stages) are covered by the whole-tree validation battery,
/// which applies this pass to every nested statement it walks.
pub fn validate_param_property_shorthand(clauses: &[Clause]) -> Result<(), EngineError> {
    for clause in clauses {
        match clause {
            Clause::Match(mc) => {
                for path in &mc.pattern {
                    check_no_param_property_shorthand_in_elements(&path.elements)?;
                }
            }
            Clause::Merge(mc) => {
                check_no_param_property_shorthand_in_elements(&mc.pattern.elements)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_no_param_property_shorthand_in_elements(
    elements: &[PatternElement],
) -> Result<(), EngineError> {
    for elem in elements {
        match elem {
            PatternElement::Node(np) => {
                if np.properties_param.is_some() {
                    return Err(EngineError::semantic(
                        ErrorKind::InvalidParameterUse,
                        "a parameter may not be used as a node predicate outside of CREATE"
                            .to_string(),
                    ));
                }
            }
            PatternElement::Relationship(rp) => {
                if rp.properties_param.is_some() {
                    return Err(EngineError::semantic(
                        ErrorKind::InvalidParameterUse,
                        "a parameter may not be used as a relationship predicate outside of CREATE"
                            .to_string(),
                    ));
                }
            }
            PatternElement::Quantified(qpp) => {
                check_no_param_property_shorthand_in_elements(&qpp.elements)?;
            }
        }
    }
    Ok(())
}
/// CIP-156: `$any(...)` is a MATCH-only disjunction quantifier for dynamic
/// label/type expressions -- Neo4j rejects it in CREATE/MERGE (GQLSTATUS
/// `42I55`) for both node labels and relationship types, since a
/// conjunction is required to unambiguously determine what to create.
/// `$(...)`/`$all(...)` remain allowed (checked at runtime by
/// the executor's `eval::labels::resolve_write_labels`).
pub(crate) fn validate_dynamic_label_no_any(patterns: &[PatternPath]) -> Result<(), EngineError> {
    for path in patterns {
        for elem in &path.elements {
            let label_expr = match elem {
                PatternElement::Node(np) => &np.label_expr,
                PatternElement::Relationship(rp) => &rp.type_expr,
                PatternElement::Quantified(_) => continue,
            };
            if let Some(le) = label_expr {
                check_no_any_quantifier(le)?;
            }
        }
    }
    Ok(())
}

fn check_no_any_quantifier(expr: &LabelExpr) -> Result<(), EngineError> {
    // The leaf rule only: the shared walker ([`walk_label_expr`]) owns the
    // `Not`/`And`/`Or` fold.
    walk_label_expr(expr, &mut |le| match le {
        LabelExpr::Dynamic(d) if d.quantifier == relate_parser::DynamicLabelQuantifier::Any => {
            Err(EngineError::semantic(
                ErrorKind::InvalidLabelExpression,
                "Dynamic labels/types using $any() are not allowed in CREATE/MERGE".to_string(),
            ))
        }
        _ => Ok(()),
    })
}

/// CIP-156: a dynamic label/type expression (`$(...)`/`$all(...)`/`$any(...)`)
/// within a CREATE/MERGE clause cannot reference any variable that this
/// same clause itself creates -- unlike ordinary property values, which may
/// freely reference an earlier-created node/relationship of the same
/// clause. MATCH is exempt (its dynamic expressions are resolved against
/// already-bound rows with no such restriction -- see
/// `EvalContext::label_expr_matches`).
pub(crate) fn validate_dynamic_label_scope(
    patterns: &[PatternPath],
    bound_vars: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    let mut created_here: IdentHashSet<SmallName> = IdentHashSet::default();
    for path in patterns {
        for elem in &path.elements {
            let var = match elem {
                PatternElement::Node(np) => &np.variable,
                PatternElement::Relationship(rp) => &rp.variable,
                PatternElement::Quantified(_) => continue,
            };
            if let Some(v) = var {
                if !bound_vars.contains(v) {
                    created_here.insert(v.clone());
                }
            }
        }
    }
    if created_here.is_empty() {
        return Ok(());
    }
    for path in patterns {
        for elem in &path.elements {
            let label_expr = match elem {
                PatternElement::Node(np) => &np.label_expr,
                PatternElement::Relationship(rp) => &rp.type_expr,
                PatternElement::Quantified(_) => continue,
            };
            if let Some(le) = label_expr {
                super::self_reference::check_dynamic_label_scope(le, &created_here)?;
            }
        }
    }
    Ok(())
}

/// Validate MERGE pattern for VariableAlreadyBound errors.
pub(crate) fn validate_merge_bindings(
    pattern: &PatternPath,
    bound_vars: &IdentHashSet<SmallName>,
    shadowable: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // Check if all node variables in the pattern are already bound
    // A MERGE with a fully-bound pattern is fine (it's matching)
    // But a MERGE with a node variable that is bound AND has new labels = error
    // Also check that relationship variables aren't already bound
    let has_rels = has_relationship(pattern);

    for elem in &pattern.elements {
        match elem {
            PatternElement::Node(np) => {
                if let Some(var) = &np.variable {
                    // An enclosing-scope name is shadowable in a FOREACH body:
                    // the MERGE declares a fresh binding for it (empty set at
                    // top level, so this is a no-op there).
                    if bound_vars.contains(var) && !shadowable.contains(var) {
                        // For MERGE, a bound node used as endpoint for relationships is ok
                        // But a bare MERGE (n) or MERGE (n:Label) where n is already bound is an error
                        if !has_rels {
                            return Err(var.with_str(EngineError::duplicate_variable));
                        }
                        // Even with rels, if it introduces new labels, that's an error
                        if np.label_expr.is_some() || !np.properties.is_empty() {
                            return Err(var.with_str(EngineError::duplicate_variable));
                        }
                    }
                }
            }
            PatternElement::Relationship(rp) => {
                if let Some(var) = &rp.variable {
                    if bound_vars.contains(var) && !shadowable.contains(var) {
                        return Err(var.with_str(EngineError::duplicate_variable));
                    }
                }
                // Check for variable-length relationships in MERGE
                if rp.is_variable_length() {
                    return Err(EngineError::semantic(
                        ErrorKind::CreatingVarLength,
                        "variable length relationships cannot be used in MERGE".to_string(),
                    ));
                }
                // Check for missing/non-single relationship type in MERGE
                if !is_single_type(&rp.type_expr) {
                    return Err(EngineError::semantic(
                        ErrorKind::NoSingleRelationshipType,
                        "MERGE relationships must have exactly one type".to_string(),
                    ));
                }
            }
            PatternElement::Quantified(_) => {
                return Err(EngineError::semantic(
                    ErrorKind::QuantifiedPathPatternNotSupported,
                    "quantified path patterns cannot be used in MERGE".to_string(),
                ));
            }
        }
    }
    Ok(())
}
