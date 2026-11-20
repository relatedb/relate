//! The constraints walk's scope carrier: one value holding every name the
//! walk has brought into scope, the variable kind it classified each
//! name to, and — inside a CALL-subquery body — the enclosing statement's
//! names, the import boundary that body's leak rule reads. Beside
//! [`super`](super)'s `VarType` vocabulary and `projected_var_types`
//! fold, so the scope concept has one home and the expression rules in
//! [`crate::validate::expressions`] and the paren-entity gate in
//! [`crate::validate::pattern`] both depend down on it.

use crate::ident_hash::{IdentHashMap, IdentHashSet};
use relate_names::SmallName;

use relate_error::EngineError;
use relate_parser::{Clause, Expr, ReturnItem};

use super::pattern_usage::bind_clause_var_kinds;
use super::{projected_var_types, VarType};
use crate::ast::collect::{collect_expr_variables, projected_names, search_score_var};

/// The constraints walk's one scope value: every name the walk has
/// brought into scope, beside the variable kind it classified the name
/// to — a name in scope with no kind entry is a value, or a projection
/// whose kind is decided per row — and, inside a CALL-subquery body, the
/// enclosing statement's names. One carrier, not a name set and a kind
/// map in hand-mirrored lockstep: [`Self::projected`] is the one
/// WITH/RETURN projection (the one skeleton's two payload
/// instantiations), the WHERE view is one [`Self::with_projection`], the
/// commit one assignment — and every kind rule (the paren-entity gate in
/// [`crate::validate::pattern`], the static-kind family in
/// [`crate::validate::expressions`]) reads [`Self::kind_of`], so a WITH
/// projection scopes them together with the name scope.
#[derive(Clone, Default)]
pub(crate) struct ExprScope {
    /// The in-scope names — the canonical name-set helpers' view through
    /// [`Self::names`].
    names: IdentHashSet<SmallName>,
    /// The classification: an entity kind or `Scalar` for a name the
    /// walk could classify, absent for an unconstrained name. A subset
    /// of `names` by construction — the dispatch's pre-arm bind step
    /// ([`Self::bind_clause`]) writes names and kinds together, from the
    /// one pattern walk.
    kinds: IdentHashMap<SmallName, VarType>,
    /// The enclosing statement's names, set only inside a CALL-subquery
    /// body (the import boundary the leak rule reads through
    /// [`Self::reject_unimported_outer_refs`]); empty at the top level
    /// and inside an expression subquery, which sees the enclosing row
    /// with no boundary to enforce.
    outer: IdentHashSet<SmallName>,
}

impl ExprScope {
    /// The in-scope name set: the canonical scope helpers'
    /// (`check_expr_vars_defined`, `check_name_defined`, …) view, and
    /// the mutable name-binding surface of the walk's name-only clause
    /// arms.
    pub(crate) fn names_mut(&mut self) -> &mut IdentHashSet<SmallName> {
        &mut self.names
    }

    /// The in-scope name set, read-only.
    pub(crate) fn names(&self) -> &IdentHashSet<SmallName> {
        &self.names
    }

    /// The kind the walk classified `name` to, when it did — `None` for
    /// an unconstrained name and for a name out of scope.
    pub(crate) fn kind_of(&self, name: &SmallName) -> Option<VarType> {
        self.kinds.get(name).copied()
    }

    /// Bind a clause's pattern variables — names and kinds together, the
    /// one pattern walk — plus a MATCH's SEARCH score variable (a name
    /// with no kind: a scalar). The dispatch calls this once per clause,
    /// before the arm: a MATCH's own WHERE reads its pattern's
    /// classification, and the nested bodies inside its expressions seed
    /// from the bound names. A clause with no pattern binds nothing.
    pub(crate) fn bind_clause(&mut self, clause: &Clause) -> Result<(), EngineError> {
        bind_clause_var_kinds(clause, &mut self.names, &mut self.kinds)?;
        if let Clause::Match(mc) = clause {
            if let Some(score_var) = search_score_var(mc) {
                self.names.insert(score_var.clone());
            }
        }
        Ok(())
    }

    /// What a clause's own expressions — its WHEREs, and the subquery
    /// bodies inside them — see: the input scope, the clause's own
    /// bindings ([`Self::bind_clause`]'s names, already bound by the
    /// dispatch), plus, for a WITH, this projection's own output. One
    /// view: the clause's expression gates and the nested-body seed
    /// derive through the same [`Self::with_projection`] policy, so the
    /// two cannot disagree on what the clause's expressions see.
    pub(crate) fn expression_view(&self, clause: &Clause) -> ExprScope {
        match clause {
            Clause::With(wc) => self.with_projection(&self.projected(&wc.items)),
            _ => self.clone(),
        }
    }

    /// The scope a subquery body nested inside a clause's expressions
    /// starts from: the clause's expression view, with the CALL import
    /// boundary cleared — an expression subquery (EXISTS/COUNT/COLLECT)
    /// sees the enclosing row, no boundary to enforce. The CALL-arm
    /// seeds ([`Self::seeded`]) carry the boundary instead.
    pub(crate) fn subquery_body_seed(&self, clause: &Clause) -> ExprScope {
        let mut view = self.expression_view(clause);
        view.outer = IdentHashSet::default();
        view
    }

    /// The scope a subquery body nested inside a WITH's projection
    /// *items* starts from: the clause's input, with the CALL boundary
    /// cleared — the items evaluate against the input, never against
    /// the projection their sibling items define (an item cannot
    /// reference a same-clause alias; that report is the position's
    /// own).
    pub(crate) fn subquery_item_seed(&self) -> ExprScope {
        let mut input = self.clone();
        input.outer = IdentHashSet::default();
        input
    }

    /// The seed for a CALL-subquery body's arm, derived from one source
    /// at the recursive boundary: `names` as the caller derived them
    /// (already restricted to what the body sees), with the caller's
    /// kind classification for exactly those names — an outer node
    /// stays a node inside the body, a name the body cannot see carries
    /// no kind into it. The import boundary is not the seed's to decide:
    /// [`Self::carry_outer_boundary`] sets it, at the call boundary the
    /// boundary policy belongs to.
    pub(crate) fn seeded(names: IdentHashSet<SmallName>, caller: &ExprScope) -> ExprScope {
        let kinds = caller
            .kinds
            .iter()
            .filter(|(name, _)| names.contains(*name))
            .map(|(name, kind)| (name.clone(), *kind))
            .collect();
        ExprScope {
            names,
            kinds,
            outer: IdentHashSet::default(),
        }
    }

    /// Arm the CALL-subquery import boundary: the enclosing statement's
    /// names, which the carrier's boundary rule
    /// ([`Self::reject_unimported_outer_refs`]) reads. Only a naked
    /// `CALL { … }` body's arms carry it — the leading importing WITH
    /// is evaluated against the outer row, and a name read past that
    /// boundary is the leak; a scope clause's imports stay visible for
    /// the whole body, so those arms carry none (the planner's binder
    /// owns unimported names there).
    pub(crate) fn carry_outer_boundary(&mut self, outer: IdentHashSet<SmallName>) {
        self.outer = outer;
    }

    /// The WITH/RETURN projection of the whole carrier — one derivation:
    /// the names through [`projected_names`], the kinds through
    /// [`projected_var_types`] (the one projection skeleton's two
    /// payload instantiations), so the name set and the kind map the
    /// following clauses see can never disagree on what the clause
    /// projected.
    pub(crate) fn projected(&self, items: &[ReturnItem]) -> ExprScope {
        ExprScope {
            names: projected_names(items, &self.names),
            kinds: projected_var_types(items, &self.kinds),
            outer: self.outer.clone(),
        }
    }

    /// The WHERE view: this scope plus a same-clause projection's own
    /// output — a same-clause alias is referenceable exactly as it is to
    /// every clause after the projection. The names add; the kinds
    /// *overlay* — every name the projection emits first loses the
    /// input's classification, then gains the projection's — so a
    /// projected name's classification, including the decision that it
    /// has none (a same-clause re-projection to a per-row expression),
    /// always supersedes the input's stale one.
    pub(crate) fn with_projection(&self, projected: &ExprScope) -> ExprScope {
        let mut names = self.names.clone();
        names.extend(projected.names.iter().cloned());
        let mut kinds = self.kinds.clone();
        kinds.retain(|name, _| !projected.names.contains(name));
        kinds.extend(
            projected
                .kinds
                .iter()
                .map(|(name, kind)| (name.clone(), *kind)),
        );
        ExprScope {
            names,
            kinds,
            outer: self.outer.clone(),
        }
    }

    /// The CALL-subquery import boundary: a name the enclosing statement
    /// bound but this body never imported is `UndefinedVariable`, not a
    /// silent read of the enclosing row (live 2026.10: a naked
    /// `CALL { … }` body sees only what its leading importing WITH
    /// names). A name bound *nowhere* is left to the planner's binder,
    /// which owns its own diagnostic. Inside an expression subquery
    /// there is no boundary ([`Self::subquery_body_seed`] clears it) —
    /// the body sees the enclosing row.
    pub(crate) fn reject_unimported_outer_refs(&self, expr: &Expr) -> Result<(), EngineError> {
        if self.outer.is_empty() {
            return Ok(());
        }
        let mut vars = IdentHashSet::default();
        collect_expr_variables(expr, &mut vars);
        let mut leaked: Vec<&SmallName> = vars
            .iter()
            .filter(|name| !self.names.contains(*name) && self.outer.contains(*name))
            .collect();
        leaked.sort();
        match leaked.first() {
            Some(name) => Err(name.with_str(EngineError::undefined_variable)),
            None => Ok(()),
        }
    }
}
