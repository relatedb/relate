//! The 42N07 shadowing rules for subquery-expression bodies (CIP-65/CIP-71/
//! CIP-213): a `WITH`/`RETURN` projection, a `NEXT` stage export, or an
//! `UNWIND`/`LOAD CSV` alias in an `EXISTS`/`COUNT`/`COLLECT` body may not
//! re-declare a name the enclosing scope already binds. Self-contained
//! beside the one policy that invokes it
//! ([`super::check_subquery_expression`]) and the CALLEE-side validation in
//! `crate::validate::subquery`.

use crate::binding::EnvBridge;
use relate_core::{Error, Result};
use relate_ir::{Clause, Expr, Query};
use relate_names::SmallName;

use crate::binding::Env;

/// Whether a subquery-expression body reads an outer variable other than
/// `shadowed` — that is, whether the body is *correlated*. A correlated body
/// has imported the enclosing scope explicitly, so it may re-bind an outer
/// name it does not read (`GroupByAcceptance` [29] "A COUNT subquery variable
/// shadowing a non-grouping variable is allowed": `COUNT { UNWIND range(1,
/// a.p) AS b RETURN b }` reads outer `a`). An uncorrelated body may not
/// shadow at all (`VariableAlreadyBound`).
fn body_imports_other_outer(query: &Query, env: &Env, shadowed: &SmallName) -> bool {
    let mut correlated = false;
    relate_ir::walk_query(query, &mut |_| {}, &mut |e| {
        if let Expr::Variable(v) = e {
            if v != shadowed && env.contains_name(v) {
                correlated = true;
            }
        }
    });
    correlated
}

/// 42N07 over a subquery-expression body's `WITH`/`RETURN` projections: a
/// non-pass-through item may not produce a name the enclosing scope already
/// binds. `RETURN x` and `RETURN x AS x` are references to the imported
/// binding (the `AllowSelfAlias` pass-through policy), so they are legal.
pub(crate) fn reject_shadowing_projections(query: &Query, imported: &[String]) -> Result<()> {
    let mut stage = Some(query);
    while let Some(query) = stage {
        for part in &query.parts {
            for clause in &part.clauses {
                let items = match clause {
                    Clause::With { items, .. } | Clause::Return { items, .. } => items,
                    _ => continue,
                };
                for item in items {
                    let Some(alias) = &item.alias else {
                        continue;
                    };
                    if !imported.iter().any(|name| alias == name.as_str()) {
                        continue;
                    }
                    let pass_through = matches!(&item.expr, Expr::Variable(v) if v == alias);
                    if !pass_through {
                        return Err(shadowing_error(alias));
                    }
                }
            }
        }
        stage = query.next.as_deref();
    }
    Ok(())
}

/// 42N07 over a subquery-expression body's projections: a mid-chain NEXT
/// stage's export and any projection item may not re-declare an imported
/// name. The two halves differ in which projection they read (a stage's
/// computed export set vs. an item's alias), not in the policy.
pub(super) fn reject_projection_shadowing(query: &Query, imported: &[String]) -> Result<()> {
    // A NEXT stage's projection is consumed by the following stage, so
    // re-projecting an *imported* name there shadows the enclosing binding
    // the chain must keep visible (CIP-213 `NextStatementAcceptance` "Should
    // fail due to redeclaration of constant in subquery expression":
    // `EXISTS { … RETURN a, b NEXT … }` is 42N07). Only mid-chain stages are
    // checked — the final stage's export is the subquery expression's own
    // value.
    let mut stage = query;
    while let Some(next) = stage.next.as_deref() {
        let exported = crate::names::parts_output_names(
            &stage.parts,
            Vec::new(),
            &std::collections::HashSet::new(),
        )?;
        if let Some(name) = exported.iter().find(|n| imported.contains(*n)) {
            return Err(shadowing_error(&SmallName::from_name(name.as_str())));
        }
        stage = next;
    }
    // 42N07: no projection item may re-declare an outer name. A bare
    // pass-through (`RETURN x`, `RETURN x AS x`) is a reference to the
    // imported binding, not a re-declaration, so it stays legal; any other
    // item producing a colliding output name shadows the outer scope
    // (`RETURN n AS x`, `WITH n.name AS x`). Neo4j applies this to every
    // stage, correlated bodies included.
    reject_shadowing_projections(query, imported)
}

/// 42N07 over a subquery-expression body's *binding* clauses: an
/// `UNWIND`/`LOAD CSV` alias may not shadow an import — unless the body also
/// reads another outer name, which makes the body correlated and the
/// binding a legitimate re-use of a per-row constant.
pub(super) fn reject_binding_shadowing(query: &Query, env: &Env) -> Result<()> {
    for part in &query.parts {
        for clause in &part.clauses {
            let introduced = match clause {
                Clause::Unwind { alias, .. }
                | Clause::LoadCsv {
                    variable: alias, ..
                } => alias,
                _ => continue,
            };
            if env.contains_name(introduced) && !body_imports_other_outer(query, env, introduced) {
                return Err(Error::syntax(
                    "VariableAlreadyBound: inner variable shadows outer scope",
                ));
            }
        }
    }
    Ok(())
}

/// The one 42N07 text.
fn shadowing_error(name: &SmallName) -> Error {
    Error::syntax(format!(
        "VariableShadowing: the variable `{name}` is shadowing a variable with the same name from the outer scope and needs to be renamed"
    ))
}
