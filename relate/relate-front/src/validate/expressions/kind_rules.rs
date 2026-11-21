//! The walk's one static-kind rule family, its own module beside the
//! clause-walk dispatch it serves.

use relate_error::{EngineError, ErrorKind};
use relate_names::SmallName;
use relate_parser::Expr;

use crate::ast::collect::{walk_expr_scoped, BinderStack, ScopedExprVisitor};
use crate::ast::tree::{unparen, var_property};
use crate::validate::scope::{ExprScope, VarType};

/// The walk's one static-kind rule family, one pass over an expression:
/// property access on a statically-known path variable (a path is not a
/// map), `labels()` on one (Graph3 [8]), and `type()` on a
/// statically-known node variable — the three statically-certain kind
/// misuses, one rule ([`reject_operand_kind`]) behind both shapes. Every
/// expression position the clause walk validates runs the same family:
/// the WITH/RETURN item lists, the MATCH's element-pattern, path-pattern
/// and own WHEREs, and the WITH's WHERE against the projection view
/// built there — no position applies a subset. The property operand
/// reads through the canonical property-on-a-variable recognizer
/// ([`var_property`]) and the builtin argument through the paren
/// transparency helper ([`unparen`]), so `(p).foo` and `labels((p))`
/// resolve through the same paren policy as the classification itself.
/// The pass rides the scope-aware walker ([`walk_expr_scoped`]): a name
/// bound by an enclosing quantifier, comprehension, `reduce`/`allReduce`
/// or pattern comprehension shadows the walk's classification of the
/// outer same-named variable (`all(p IN nodes(p) WHERE …labels(p)…)`
/// reads the bound element, never the path) — a shadowed name is a
/// variable the walk cannot decide, so like every other undecidable
/// shape it stays with the runtime type check. The shadow set is this
/// walk's own: it does not cross a subquery expression's boundary — the
/// body's seed keeps the outer kinds, so a name shadowed in the
/// enclosing expression still reads the outer classification inside an
/// `EXISTS`/`COUNT`/`COLLECT` body (a pre-existing gap of the seed
/// model, tracked as its own ticket). Relate has no general type
/// inferencer, so each rule stays scoped to the shape it can decide (a
/// variable the walk classified and no binder shadows); anything else
/// (a variable that might be fine, a property access on an unconstrained
/// name, `labels()` on a relationship — the executor's own coercion
/// rejects it) is left to the runtime type check.
pub(super) fn check_static_kind_rules(expr: &Expr, scope: &ExprScope) -> Result<(), EngineError> {
    let mut visit = KindRuleVisit {
        scope,
        shadowed: BinderStack::default(),
    };
    walk_expr_scoped(expr, &mut visit)
}

/// The kind-rule family's scoped visit: both shapes checked on every
/// node, against the binder stack the walker's events own. A shadowed
/// name skips the rule — the binder's variable is what the expression
/// reads, and its kind is the runtime's to check.
struct KindRuleVisit<'a> {
    scope: &'a ExprScope,
    shadowed: BinderStack,
}

impl KindRuleVisit<'_> {
    /// The family's one guarded application: a binder in scope shadows
    /// `name` (the expression reads the binder's variable, whose kind is
    /// the runtime's to check), else the kind rejection runs.
    fn reject(&self, name: &SmallName, kind: VarType, message: &str) -> Result<(), EngineError> {
        if self.shadowed.shadows(name) {
            return Ok(());
        }
        reject_operand_kind(name, kind, message, self.scope)
    }

    /// The family's one shape classifier: the operand a statically-known
    /// variable kind disproves — a property access on a path variable
    /// (through the canonical recognizer) or the argument of the
    /// one-argument builtins (`labels()` on a path, `type()` on a node),
    /// through the paren-transparency helper so `labels((p))` resolves
    /// like `labels(p)` — with the kind it disproves and the message it
    /// rejects with, one pairing per shape behind the one rule.
    fn disproved_operand(e: &Expr) -> Option<(&SmallName, VarType, &'static str)> {
        if let Some((v, _)) = var_property(e) {
            return Some((v, VarType::Path, "property access is not allowed on paths"));
        }
        let Expr::FunctionCall(fc) = e else {
            return None;
        };
        if fc.args.len() != 1 {
            return None;
        }
        let (kind, message) = fc.name.with_str(|n| match n.to_lowercase().as_str() {
            "labels" => Some((VarType::Path, "labels() requires a node, got PATH")),
            "type" => Some((
                VarType::Node,
                "type() can only be applied to relationships, not nodes",
            )),
            _ => None,
        })?;
        let Expr::Variable(v) = unparen(&fc.args[0]) else {
            return None;
        };
        Some((v, kind, message))
    }
}

impl ScopedExprVisitor for KindRuleVisit<'_> {
    fn enter_expr(&mut self, e: &Expr) -> Result<(), EngineError> {
        // One classifier, one guarded application — the family's whole
        // visit.
        if let Some((v, kind, message)) = Self::disproved_operand(e) {
            return self.reject(v, kind, message);
        }
        Ok(())
    }

    fn bind_name(&mut self, name: &SmallName) -> Result<(), EngineError> {
        self.shadowed.bind(name);
        Ok(())
    }

    fn unbind_name(&mut self) -> Result<(), EngineError> {
        self.shadowed.unbind();
        Ok(())
    }
}

/// The family's one rule: a variable the walk classified to `kind` is
/// rejected with `message` — the kind equality test and the rejection
/// exist once, behind both of the family's shapes (the property access's
/// base, resolved through [`var_property`], and the builtin's argument,
/// resolved through [`unparen`]).
fn reject_operand_kind(
    name: &SmallName,
    kind: VarType,
    message: &str,
    scope: &ExprScope,
) -> Result<(), EngineError> {
    if scope.kind_of(name) == Some(kind) {
        return Err(EngineError::semantic(
            ErrorKind::InvalidArgumentType,
            message.to_string(),
        ));
    }
    Ok(())
}
