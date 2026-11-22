//! The projection-item naming rule: the output name a WITH/RETURN item
//! projects and the implicit-name vocabulary its alias gates read — the
//! one home of the family (REL8-1866's unaliased map projection naming
//! included), split out of [`super::collect`] beside the collectors it
//! serves. [`effective_name`] is the name rule every scope projection
//! renders ([`super::collect::projected_names`] folds it over an item
//! list); [`item_needs_alias`] is the alias-requirement predicate the
//! WITH and CALL-subquery/brace 42N21 gates call; [`effective_expr`]
//! owns the paren-transparency policy the family classifies through.
//! The IR twin of the implicit-name vocabulary is
//! `relate_ir::canonical_name` (the planner's alias gate reads it);
//! this module's tests pin the two spellings' agreement.

use relate_names::SmallName;

use relate_parser::{Expr, ReturnItem};

use super::tree::unparen;

/// The paren-transparent form of a projection item's expression — the one
/// home of the paren-transparency policy (LABS-1615/1616): redundant
/// `Expr::Paren` layers change nothing about an item, because the lowering
/// unwraps them and the IR's `Projection::output_name` therefore always
/// judges the unparenthesized form. Every consumer that *classifies* an
/// item's expression — names, alias requirements, node-safety, restatement
/// matching, import classification — reads it through here, never
/// `item.expr` directly; a consumer that walks or lowers the whole
/// expression keeps the raw form (parens are transparent to walkers by
/// descent, and the lowering owns the unwrap). The `tree` shape
/// recognizers this module's own derivations hand the form to
/// (`Expr::map_projection_var` and its kin) unparen again internally — a
/// no-op over the already-unwrapped form that keeps the
/// one-entry-point invariant true by construction.
pub(crate) fn effective_expr(item: &ReturnItem) -> &Expr {
    unparen(&item.expr)
}

/// The item's bare variable, when its (paren-transparent) expression is
/// one — the "this item is the variable `n`" model every alias gate,
/// import classifier and node-safety fold reads, spelled once beside
/// [`effective_expr`] instead of re-matched at each site (LABS-1615/1616).
pub(crate) fn item_variable(item: &ReturnItem) -> Option<&SmallName> {
    match effective_expr(item) {
        Expr::Variable(v) => Some(v),
        _ => None,
    }
}

/// The implicit name an unaliased projection item carries: its bare
/// variable, or — a map projection on a variable being a view of that
/// variable, which carries the variable's name (matching the executor's
/// `Projection::output_name`, and what a CALL subquery exports for the
/// `RETURN n {.prop}` shape; Neo4j 2026.10 live-confirmed, REL8-1866) —
/// the projected variable. The one derivation of the implicit-name
/// vocabulary: [`item_needs_alias`]'s gate predicate and
/// [`effective_name`]'s unaliased arm both read it, so the shape list is
/// spelled once — both arms classify through [`effective_expr`], the one
/// entry point. The IR twin of this vocabulary is
/// [`relate_ir::canonical_name`] (the planner's alias gate reads it);
/// the drift test in this module's tests pins their agreement. A
/// property access is deliberately absent — it names a column top-level
/// (by verbatim source text) yet stays an expression every alias gate
/// rejects.
pub(crate) fn implicit_name(item: &ReturnItem) -> Option<&SmallName> {
    item.implicit_name()
}

/// Whether a projection item needs an explicit `AS` to name its column:
/// it has no alias, and no implicit name ([`implicit_name`]). The one
/// alias-requirement predicate — WITH's gate
/// (`validate::structure::check_projected_names`) and the
/// CALL-subquery/brace 42N21 gate (`call_scope::check_return_items_aliased`)
/// both call it, so the policy the planner's IR twin spells once
/// (`relate_ir::canonical_name` via the planner's
/// `is_unaliased_expression`) lives once here too.
pub(crate) fn item_needs_alias(item: &ReturnItem) -> bool {
    item.alias.is_none() && implicit_name(item).is_none()
}

pub(crate) fn effective_name(item: &ReturnItem) -> Option<SmallName> {
    // The name rule's one home is `ReturnItem::output_name` in relate-parser
    // (alias, else the implicit name, else the verbatim source text, which
    // mirrors the IR `Projection::output_name` by construction --
    // [`relate_ir::canonical_name`]'s doc owns the property-spelling facts
    // behind the last arm, so two spellings of one property are two
    // distinct columns and only an exact-text duplicate conflicts).
    item.output_name()
}

#[cfg(test)]
pub(crate) mod tests;
