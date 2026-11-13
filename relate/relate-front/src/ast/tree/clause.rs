//! The clause-level expression descent of the whole-tree walker: every
//! expression a [`Clause`] carries, routed through the shared expression
//! walker ([`walk_expr_at`]) so the ADR-0257 depth cap and the dynamic
//! operand visit cover clause expressions too.

use relate_error::EngineError;
use relate_parser::Clause;

use crate::ast::search_spec::walk_search_spec;
use crate::ast::stmt_ref::StmtRef;
use crate::show_columns::catalog_info;

use super::{
    enter_substatement, walk_expr_at, walk_label_expr_tree, walk_page_stages_tree,
    walk_path_exprs_tree, walk_projection_exprs_tree, walk_property_access_tree,
    walk_set_item_exprs_tree, Depths, LabelSite, TreeWalk,
};

pub(super) fn walk_clause_exprs_tree(
    clause: &Clause,
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    match clause {
        Clause::Match(mc) => {
            for path in &mc.pattern {
                walk_path_exprs_tree(path, depths, walk)?;
            }
            if let Some(search) = &mc.search {
                // Every expression of the (possibly nested, CIP-278) source
                // specification, through the one shared enumeration --
                // straight into the depth walk, an over-deep expression
                // stopping it.
                walk_search_spec(&search.spec, &mut |_| Ok(()), &mut |_, expr| {
                    walk_expr_at(expr, depths, walk)
                })?;
            }
            if let Some(w) = &mc.where_expr {
                walk_expr_at(w, depths, walk)?;
            }
        }
        Clause::Create(cc) => {
            for path in &cc.pattern {
                walk_path_exprs_tree(path, depths, walk)?;
            }
        }
        Clause::Merge(mc) => {
            walk_path_exprs_tree(&mc.pattern, depths, walk)?;
            for item in mc.on_create.iter().chain(mc.on_match.iter()) {
                walk_set_item_exprs_tree(item, depths, walk)?;
            }
        }
        Clause::Set(sc) => {
            for item in &sc.items {
                walk_set_item_exprs_tree(item, depths, walk)?;
            }
        }
        Clause::Remove(rc) => {
            for item in &rc.items {
                if let Some(p) = &item.property {
                    walk_property_access_tree(p, depths, walk)?;
                }
                for label in &item.labels {
                    walk_label_expr_tree(label, depths, walk, LabelSite::PatternOrWrite)?;
                }
            }
        }
        Clause::Delete(dc) => {
            for e in &dc.expressions {
                walk_expr_at(e, depths, walk)?;
            }
        }
        Clause::Return(rc) => {
            walk_projection_exprs_tree(
                &rc.items,
                &rc.order_by,
                rc.skip.as_ref(),
                rc.limit.as_ref(),
                depths,
                walk,
            )?;
        }
        Clause::With(wc) => {
            walk_projection_exprs_tree(
                &wc.items,
                &wc.order_by,
                wc.skip.as_ref(),
                wc.limit.as_ref(),
                depths,
                walk,
            )?;
            if let Some(w) = &wc.where_expr {
                walk_expr_at(w, depths, walk)?;
            }
        }
        Clause::Unwind(uw) => {
            walk_expr_at(&uw.expr, depths, walk)?;
        }
        Clause::CallProcedure(cp) => {
            for arg in &cp.args {
                walk_expr_at(arg, depths, walk)?;
            }
            if let Some(w) = &cp.where_expr {
                walk_expr_at(w, depths, walk)?;
            }
        }
        Clause::CallSubquery(csq) => {
            if let Some(cit) = &csq.in_transactions {
                // Every expression of the `IN TRANSACTIONS` tail, through the
                // parser's one slot enumeration (`InTransactionsClause::exprs`):
                // the concurrency value, the batch size, the retry duration and
                // the `DISJOINT BY` keys.
                for expr in cit.exprs() {
                    walk_expr_at(expr, depths, walk)?;
                }
            }
            enter_substatement(
                walk,
                StmtRef::Real(&csq.statement),
                depths.deeper_statement(),
            )?;
        }
        Clause::StandaloneOrderBy(page) => {
            walk_page_stages_tree(&page.stages, depths, walk)?;
        }
        Clause::Foreach(f) => {
            walk_expr_at(&f.list_expr, depths, walk)?;
            // A FOREACH body is a clause list, not a `Statement`; present it
            // to the statement visitor as a borrowed body one nesting level
            // deeper so per-statement validators (and the depth cap) cover
            // it. It inherits this statement's entry expression depth: the
            // list expression has returned by the time lowering enters the
            // body.
            enter_substatement(walk, StmtRef::Body(&f.clauses), depths.deeper_statement())?;
        }
        Clause::Conditional(cc) => {
            for branch in &cc.branches {
                walk_expr_at(&branch.predicate, depths, walk)?;
                enter_substatement(walk, StmtRef::Real(&branch.body), depths.deeper_statement())?;
            }
            if let Some(else_body) = &cc.else_body {
                enter_substatement(walk, StmtRef::Real(else_body), depths.deeper_statement())?;
            }
        }
        Clause::Use(uc) => {
            walk_expr_at(&uc.graph_expr, depths, walk)?;
        }
        Clause::LoadCSV(lc) => {
            walk_expr_at(&lc.source, depths, walk)?;
        }
        Clause::CreateIndex(ix) => walk_option_values(&ix.options, depths, walk)?,
        Clause::CreateConstraint(cc) => walk_option_values(&cc.options, depths, walk)?,
        // Every catalog command, through the one descriptor: its argument
        // expressions, WHERE and YIELD paging stages are the only expressions
        // it carries, so a new SHOW command's expression cannot escape the
        // depth walk. This pattern names exactly the variants
        // `catalog_info` classifies (its exhaustive match is a compile error
        // until a new clause variant is decided there), so the descriptor
        // lookup below cannot fail.
        Clause::ShowFunctions(_)
        | Clause::ShowSettings(_)
        | Clause::ShowTransactions(_)
        | Clause::TerminateTransactions(_)
        | Clause::ShowConstraints(_)
        | Clause::ShowIndexes(_)
        | Clause::ShowDatabases(_)
        | Clause::ShowRoles(_)
        | Clause::ShowServers(_)
        | Clause::ShowUsers(_)
        | Clause::ShowProcedures(_)
        | Clause::ShowCurrentGraphType(_)
        | Clause::ShowGraphType(_) => {
            let info = catalog_info(clause)
                .expect("the arm's pattern enumerates exactly catalog_info's Some variants");
            for argument in &info.arguments {
                walk_expr_at(argument, depths, walk)?;
            }
            if let Some(w) = info.where_expr {
                walk_expr_at(w, depths, walk)?;
            }
            walk_page_stages_tree(info.stages, depths, walk)?;
        }
        // No clause-level expression this walk owns: the DROP forms, ALTER
        // DATABASE, the empty-result terminator, and the two graph-type DDL
        // clauses whose bodies the graph-type gate validates.
        Clause::DropIndex(_)
        | Clause::DropConstraint(_)
        | Clause::AlterDatabase(_)
        | Clause::CreateGraphType(_)
        | Clause::AlterCurrentGraphType(_)
        | Clause::Finish => {}
    }
    Ok(())
}

/// Walk the value expression of every `OPTIONS { key: value }` pair — the
/// one option-body walk shared by both schema-DDL clauses, whose `options`
/// fields have the identical shape.
fn walk_option_values(
    options: &relate_parser::OptionsForm,
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    if let Some(entries) = options.literal() {
        for (_, value) in entries {
            walk_expr_at(value, depths, walk)?;
        }
    }
    Ok(())
}
