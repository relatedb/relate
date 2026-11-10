//! LABS-709: compile-time validation of *statically known* dynamic
//! property-key and label/type operands (CIP-131 bracket targets,
//! CIP-156/CIP-162 dynamic labels/types).
//!
//! Neo4j folds constant dynamic operands at compile time, so a bad constant
//! fails even when the driving `MATCH` matches zero rows; Relate's checks
//! otherwise only run per row. The fold is split the way Neo4j splits it:
//! an inline literal's value *and* content are compile-time facts, while an
//! operand reaching the site through a variable or parameter is typed
//! statically (a wrong scalar type is a compile-time `SyntaxError`) but its
//! value — `NULL`, a list's elements — is the executor's per-row
//! `TypeError`. Label *predicates* (`n:$(…)` in WHERE or a projection) take
//! no compile-time check at all. The type and token-name rule itself lives
//! in `relate_core::dynamic` and is shared with the executor's runtime
//! resolution (`relate-executor/src/eval/labels.rs`), so neither layer
//! restates a message or a type rule.
//!
//! The AST descent is not re-derived here: the whole-tree walker
//! ([`crate::ast::tree::walk_statement_tree_with_operands`]) owns the
//! knowledge of where label expressions and write-site dynamic keys sit and
//! fires [`validate_operand`] at each of them, after the enclosing
//! statement's validation battery (so the historical first-error precedence
//! is unchanged).
//!
//! "Statically known" answers two questions. The *type* is decided from the
//! static type lattice: literals (list and map literals included), signed
//! literals, arithmetic — mixed Int/Float arithmetic and division included —
//! and comparisons/boolean operators, whose result types are fixed, plus the
//! fixed result type of a constant-argument constructor call
//! (`point({x: 3, y: 0})`). The *value* needs the narrower fold
//! ([`constant_value`], beside the lattice in [`super::static_types`]),
//! which is what the content rules (empty/NUL names) consume. An operand
//! that is only known at execution — a variable bound to runtime data, a
//! call over runtime data, an unbound parameter — stays runtime-checked
//! exactly as before, so a legitimately dynamic key/label keeps working.
//!
//! Two admission-time constants extend the fold's reach to what Neo4j also
//! treats as known before execution: a variable whose binding is itself
//! statically known (`WITH 1 + 2 AS a … SET n[a] = 1`) and a bound
//! parameter (`SET n[$a] = 1`). [`validate_scoped_operands`] walks the
//! clause list in order and types those operands against the bindings
//! visible at that point, so a bad one fails on a zero-row `MATCH` too. A
//! map literal is rejected by its type alone: the rule never reads (nor
//! fabricates) its entries, wherever the literal sits — at the operand or
//! as a list member.

use crate::ident_hash::{IdentHashMap, IdentHashSet};
use std::collections::BTreeMap;

use relate_core::{bare_type_name, DynamicAccessError, DynamicNameToken, Value};
use relate_error::EngineError;
use relate_names::SmallName;
use relate_parser::{
    Clause, Expr, LabelExpr, PatternElement, PatternPath, PropertyAccess, SetItem, Statement,
};

use super::static_types::{constant_value, static_literal_type, FoldedConstant, StaticLitType};
use crate::ast::collect::{bind_pattern_vars, effective_name};
use crate::ast::tree::walk_label_expr;

/// A dynamic operand's statically known value or type. The value form is
/// checked by the shared core rule, so its exact type *and* list-element
/// types are the core's own; the named form is the type spelling for an
/// operand that is statically typed but has no materialized value (a map
/// literal, a mixed-numeric or boolean expression, a constructor call).
///
/// [`StaticOperand::Bound`] is a value reaching the operand through a
/// variable or a bound parameter. Neo4j types such an operand from its
/// static type but does not fold its *value*: a wrong scalar type is a
/// compile-time type mismatch (`SyntaxError`), while `NULL` and list
/// elements are the executor's per-row `TypeError` (a null binding on a
/// zero-row `MATCH` is not an error at all). The inline
/// [`StaticOperand::Value`] is a literal the front folds itself, so its
/// content is checked here exactly as Neo4j folds a literal.
#[derive(Clone)]
enum StaticOperand {
    Value(Value),
    Bound(Value),
    Named(&'static str),
}

/// A binding's operand form: what a variable or parameter is once stored in
/// the scope. Every stored operand is *bound* — an inline literal the front
/// folded does not survive into a binding — so the conversion is a `From` at
/// the map insert and a call site cannot forget it.
#[derive(Clone)]
enum BoundOperand {
    Value(Value),
    Named(&'static str),
}

impl From<StaticOperand> for BoundOperand {
    fn from(operand: StaticOperand) -> Self {
        match operand {
            StaticOperand::Value(value) | StaticOperand::Bound(value) => Self::Value(value),
            StaticOperand::Named(name) => Self::Named(name),
        }
    }
}

impl From<BoundOperand> for StaticOperand {
    fn from(operand: BoundOperand) -> Self {
        match operand {
            BoundOperand::Value(value) => Self::Bound(value),
            BoundOperand::Named(name) => Self::Named(name),
        }
    }
}

/// One statement body's admission-time operand environment: the operand
/// types of variables bound by earlier clauses, and the statement's bound
/// parameters. Both are constants at admission, which is why Neo4j rejects a
/// bad dynamic operand of either form even when the driving `MATCH` matches
/// no rows — the per-row runtime check would never run.
#[derive(Clone)]
struct OperandEnv<'a> {
    vars: IdentHashMap<SmallName, BoundOperand>,
    params: Option<&'a BTreeMap<String, Value>>,
}

impl<'a> OperandEnv<'a> {
    /// The environment of a caller that has no bindings and no parameters:
    /// the whole-tree walk, which only sees the operand's own text.
    fn unbound() -> Self {
        Self {
            vars: IdentHashMap::default(),
            params: None,
        }
    }
}

/// The whole-tree walk's dynamic operand visit: reject an operand whose
/// statically known value the shared core rule would reject at runtime.
///
/// The operand's *value* is decided by the fold (literals, list and map
/// literals, constant arithmetic); its *type* by the static type lattice for
/// a constant expression the fold declines (mixed numeric arithmetic,
/// division, comparisons, boolean operators) and by the fixed result type of
/// a constant-argument constructor call (`point({x: 3, y: 0})`). The
/// *content* (empty / NUL names) needs a materialized value and is only
/// checked when one was produced, always through the shared core rule.
pub(crate) fn validate_operand(expr: &Expr, token: DynamicNameToken) -> Result<(), EngineError> {
    check_operand(expr, token, &OperandEnv::unbound())
}

/// Reject an operand from `env`'s statically known bindings: a variable
/// projected by an earlier clause or a bound parameter. Walks the clause
/// list in order and carries the operand types of the bindings made so far,
/// so a name rebound by a later `WITH`/`UNWIND`/`CALL` stops being typed at
/// exactly the point Neo4j's own binding does. Nested `CALL { }` and
/// `FOREACH` bodies are walked with their enclosing (correlated) bindings;
/// expression subqueries (`EXISTS`/`COUNT`/`COLLECT`) stay opaque leaves,
/// the same boundary `collect_expr_variables` draws.
pub(crate) fn validate_scoped_operands(
    stmt: &Statement,
    params: &BTreeMap<String, Value>,
) -> Result<(), EngineError> {
    let mut env = OperandEnv {
        vars: IdentHashMap::default(),
        params: Some(params),
    };
    walk_statement_operands(stmt, &mut env)
}

/// The statement positions the rule must reach: the statement's own clauses,
/// its UNION arms, its CIP-213 NEXT stages, and (through
/// [`walk_clause_operands`]) the WHEN/ELSE bodies of a top-level
/// `Conditional`. Every one of them is lowered and executed, so a write site
/// inside one must be admitted by the same rule — the clause-only walk
/// silently skipped them.
fn walk_statement_operands(stmt: &Statement, env: &mut OperandEnv<'_>) -> Result<(), EngineError> {
    walk_clause_operands(&stmt.clauses, env)?;
    // Each UNION arm is its own statement scope, seeing the head's bindings;
    // the arm's own `union` chain recurses here.
    if let Some(union) = stmt.union.as_deref() {
        let mut arm_env = env.clone();
        walk_statement_operands(&union.statement, &mut arm_env)?;
    }
    // A NEXT stage sees the previous stage's bindings.
    if let Some(next) = &stmt.next {
        let mut next_env = env.clone();
        walk_statement_operands(next, &mut next_env)?;
    }
    Ok(())
}

fn check_operand(
    expr: &Expr,
    token: DynamicNameToken,
    env: &OperandEnv<'_>,
) -> Result<(), EngineError> {
    match static_operand(expr, env) {
        Some(StaticOperand::Value(value)) => token.check(&value).map_err(dynamic_access_error),
        Some(StaticOperand::Bound(value)) => check_bound_operand(token, &value),
        Some(StaticOperand::Named(type_name)) => {
            if token.accepts_type(type_name) {
                Ok(())
            } else {
                Err(dynamic_access_error(DynamicAccessError::wrong_type(
                    token, type_name,
                )))
            }
        }
        // Unknown at admission: left to the executor's per-row rule, so a
        // legitimately dynamic key/label keeps working.
        None => Ok(()),
    }
}

/// The admission rule for an operand reaching the write site through a
/// variable or parameter. Neo4j types it statically (a wrong scalar type is
/// a compile-time `SyntaxError`) but leaves its value to the per-row check:
/// a `NULL` operand, and every list element, is the executor's `TypeError`.
/// The accepted classes are the core token's own
/// ([`DynamicNameToken::accepts_type`]); only the value's class and the
/// content deferral live here.
fn check_bound_operand(token: DynamicNameToken, value: &Value) -> Result<(), EngineError> {
    let accepted = match value {
        // NULL is statically assignable to the accepted classes, so only the
        // executor can reject it (`SET n[c]` with a null key / label).
        Value::Null => return Ok(()),
        // A bound STRING's content (empty / NUL) and a bound list's elements
        // are per-row facts; only the type class is decided here.
        Value::String(_) => token.accepts_type(StaticLitType::String.name()),
        Value::List(_) => token.accepts_type(StaticLitType::List.name()),
        _ => false,
    };
    if accepted {
        Ok(())
    } else {
        Err(dynamic_access_error(DynamicAccessError::wrong_type(
            token,
            bare_type_name(value),
        )))
    }
}

/// An expression's statically known operand value or type, resolved through
/// `env` for a variable or parameter and from the expression itself
/// otherwise.
fn static_operand(expr: &Expr, env: &OperandEnv<'_>) -> Option<StaticOperand> {
    match expr {
        Expr::Paren(inner) => static_operand(inner, env),
        Expr::Variable(name) => env.vars.get(name).map(|bound| bound.clone().into()),
        Expr::Parameter(name) => env
            .params
            .and_then(|params| params.get(name.as_str()))
            .map(|value| StaticOperand::Bound(value.clone())),
        _ => match constant_value(expr) {
            Some(FoldedConstant::Value(value)) => Some(StaticOperand::Value(value)),
            Some(FoldedConstant::Map) => Some(StaticOperand::Named(StaticLitType::Map.name())),
            None => static_literal_type(expr)
                .map(|ty| StaticOperand::Named(ty.name()))
                .or_else(|| constant_function_type_name(expr).map(StaticOperand::Named)),
        },
    }
}

/// The result type of a built-in function whose return type is fixed by the
/// function itself, independent of its argument types. Only functions whose
/// fixed result is *not* `STRING` are listed: a fixed `STRING` result can
/// never violate the dynamic-name rule, so typing it would add a table row
/// with no effect. The spellings mirror the core type vocabulary's
/// (`relate_core::typename::bare_type_name`), which is what the runtime
/// channel renders for a folded value.
fn fixed_function_type_name(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "date" => "DATE",
        "localtime" => "LOCAL TIME",
        "time" => "ZONED TIME",
        "localdatetime" => "LOCAL DATETIME",
        "datetime" => "ZONED DATETIME",
        "duration" => "DURATION",
        "point" => "POINT",
        "tointeger" | "tointegerornull" => "INTEGER",
        "tofloat" | "tofloatornull" => "FLOAT",
        "toboolean" | "tobooleanornull" => "BOOLEAN",
        "size" | "length" => "INTEGER",
        "rand" => "FLOAT",
        _ => return None,
    })
}

/// The result type of a call whose arguments are all statically known, or
/// `None` when the call's result is not statically decided. Only
/// constant-argument calls are typed: a call over runtime data keeps its
/// per-row semantics and its own argument-type errors (`point(n.p)` reports
/// the bad argument, not a bad operand, exactly as Neo4j does).
fn constant_function_type_name(expr: &Expr) -> Option<&'static str> {
    let Expr::FunctionCall(call) = expr else {
        return None;
    };
    let name = call.name.with_str(fixed_function_type_name)?;
    call.args
        .iter()
        .all(|arg| constant_value(arg).is_some() || static_literal_type(arg).is_some())
        .then_some(name)
}

fn dynamic_access_error(error: DynamicAccessError) -> EngineError {
    EngineError::semantic(error.kind(), error.to_string())
}

/// The clause-ordered half of [`validate_scoped_operands`]: check each
/// clause's pattern and write-site dynamic operands against the bindings
/// visible at that point, then extend (or drop) the bindings the clause
/// itself makes.
fn walk_clause_operands(clauses: &[Clause], env: &mut OperandEnv<'_>) -> Result<(), EngineError> {
    for clause in clauses {
        match clause {
            Clause::Match(mc) => {
                for path in &mc.pattern {
                    check_path_operands(path, env)?;
                }
                // A pattern's own variables are entities (or fresh), so
                // their operand types are unknown from here on.
                rebind_paths(&mc.pattern, env);
            }
            Clause::Create(cc) => {
                for path in &cc.pattern {
                    check_path_operands(path, env)?;
                }
                rebind_paths(&cc.pattern, env);
            }
            Clause::Merge(mc) => {
                check_path_operands(&mc.pattern, env)?;
                for item in mc.on_create.iter().chain(&mc.on_match) {
                    check_set_item_operands(item, env)?;
                }
                rebind_paths(std::slice::from_ref(&mc.pattern), env);
            }
            Clause::Set(sc) => {
                for item in &sc.items {
                    check_set_item_operands(item, env)?;
                }
            }
            Clause::Remove(rc) => {
                for item in &rc.items {
                    check_property_key(item.property.as_ref(), env)?;
                    for label in &item.labels {
                        check_label_operands(label, env)?;
                    }
                }
            }
            Clause::With(wc) => {
                // `WITH` projects against the pre-clause bindings; the
                // aliases it introduces replace the scope wholesale.
                let mut next = IdentHashMap::default();
                for item in &wc.items {
                    if item.all {
                        next.extend(env.vars.clone());
                        continue;
                    }
                    let Some(alias) = effective_name(item) else {
                        continue;
                    };
                    if let Some(operand) = static_operand(&item.expr, env).map(BoundOperand::from) {
                        next.insert(alias, operand);
                    }
                }
                env.vars = next;
            }
            Clause::Unwind(uw) => {
                env.vars.remove(&uw.alias);
            }
            Clause::LoadCSV(lc) => {
                env.vars.remove(&lc.alias);
            }
            Clause::CallProcedure(cp) => {
                for name in cp.yields.effective_names() {
                    env.vars.remove(&name);
                }
            }
            Clause::CallSubquery(csq) => {
                let mut inner = env.clone();
                // The whole nested statement, not just its own clauses: a
                // CALL body can carry UNION arms and NEXT stages, whose write
                // sites must be admitted by the same rule.
                walk_statement_operands(&csq.statement, &mut inner)?;
                // Only names the subquery newly binds can escape into the
                // enclosing scope; a name already bound there would be a
                // `VariableAlreadyBound` error, checked by the scope
                // validator.
                for (name, operand) in inner.vars {
                    env.vars.entry(name).or_insert(operand);
                }
            }
            Clause::Foreach(f) => {
                let mut inner = env.clone();
                inner.vars.remove(&f.variable);
                walk_clause_operands(&f.clauses, &mut inner)?;
            }
            // CIP-135 `WHEN … THEN <statement>` / `ELSE <statement>`:
            // executed subquery positions, walked with the enclosing
            // bindings.
            Clause::Conditional(conditional) => {
                for branch in &conditional.branches {
                    let mut inner = env.clone();
                    walk_statement_operands(&branch.body, &mut inner)?;
                }
                if let Some(body) = &conditional.else_body {
                    let mut inner = env.clone();
                    walk_statement_operands(body, &mut inner)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_set_item_operands(item: &SetItem, env: &OperandEnv<'_>) -> Result<(), EngineError> {
    check_property_key(item.property.as_ref(), env)?;
    for label in &item.labels {
        check_label_operands(label, env)?;
    }
    // `SET labels(n) = <expr>` resolves its value through the dynamic-label
    // rule, exactly like a dynamic label atom.
    if item.labels_function.is_some() {
        if let Some(value) = &item.value {
            check_operand(value, DynamicNameToken::Label, env)?;
        }
    }
    Ok(())
}

fn check_property_key(
    property: Option<&PropertyAccess>,
    env: &OperandEnv<'_>,
) -> Result<(), EngineError> {
    let Some(key) = property.and_then(|p| p.key_expr.as_ref()) else {
        return Ok(());
    };
    check_operand(key, DynamicNameToken::PropertyKey, env)
}

fn check_label_operands(label: &LabelExpr, env: &OperandEnv<'_>) -> Result<(), EngineError> {
    // The leaf rule only: the shared walker ([`walk_label_expr`]) owns the
    // `Not`/`And`/`Or` fold.
    walk_label_expr(label, &mut |le| match le {
        LabelExpr::Dynamic(d) => check_operand(&d.expr, DynamicNameToken::Label, env),
        _ => Ok(()),
    })
}

fn check_path_operands(path: &PatternPath, env: &OperandEnv<'_>) -> Result<(), EngineError> {
    for element in &path.elements {
        check_element_operands(element, env)?;
    }
    Ok(())
}

fn check_element_operands(
    element: &PatternElement,
    env: &OperandEnv<'_>,
) -> Result<(), EngineError> {
    match element {
        PatternElement::Node(node) => {
            if let Some(label) = &node.label_expr {
                check_label_operands(label, env)?;
            }
        }
        PatternElement::Relationship(rel) => {
            if let Some(ty) = &rel.type_expr {
                check_label_operands(ty, env)?;
            }
        }
        PatternElement::Quantified(quantified) => {
            for inner in &quantified.elements {
                check_element_operands(inner, env)?;
            }
        }
    }
    Ok(())
}

/// Drop the operand types of every variable a pattern binds.
fn rebind_paths(paths: &[PatternPath], env: &mut OperandEnv<'_>) {
    let mut bound = IdentHashSet::default();
    bind_pattern_vars(paths, &mut bound);
    for name in bound {
        env.vars.remove(&name);
    }
}

#[cfg(test)]
mod tests;
