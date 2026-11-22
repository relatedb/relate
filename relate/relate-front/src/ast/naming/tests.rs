//! The naming module's tests, offloaded by the file-size gate: the
//! paren-transparency pins of the naming rule and the front↔IR drift
//! pin of the implicit-name vocabulary.

use super::*;
use relate_parser::Clause;

/// The paren-family test fixtures — one spelling of the
/// item/var/paren/prop constructor vocabulary (pub(crate): the
/// aggregation battery's projected-item test reads the same shapes
/// instead of re-spelling them, the ast/tree.rs tests-module precedent).
pub(crate) fn paren_item(expr: Expr, alias: Option<SmallName>, source_text: &str) -> ReturnItem {
    ReturnItem {
        expr,
        alias,
        all: false,
        source_text: Some(source_text.into()),
    }
}

/// A bare variable item shape's variable, named.
pub(crate) fn paren_var(name: &str) -> Expr {
    Expr::Variable(SmallName::from_name(name))
}

/// A `v.key` static property access on the variable `name`.
pub(crate) fn paren_prop(name: &str, key: &str) -> Expr {
    Expr::Property(Box::new(relate_parser::PropertyAccess {
        expr: paren_var(name),
        property: SmallName::from_name(key),
        key_expr: None,
    }))
}

/// The redundant-paren wrapper, one level.
pub(crate) fn paren_wrap(e: Expr) -> Expr {
    Expr::Paren(Box::new(e))
}

/// A dynamic-key property access `name[key]` in expression position —
/// the parser's `Expr::Subscript` shape (the `key_expr` Property form is
/// the SET/REMOVE *target* spelling, a different grammar position).
pub(crate) fn paren_subscript(name: &str, key: Expr) -> Expr {
    Expr::Subscript(Box::new(relate_parser::SubscriptExpr {
        expr: paren_var(name),
        index: Some(key),
        from: None,
        to: None,
        slice: false,
    }))
}

// LABS-1615/1616's paren family: redundant parens around a projected
// variable are transparent to the name and node-safety rules — the
// lowered item is the bare variable, so `WITH (n)` projects the column
// `n` (the same name the IR's `Projection::output_name` gives it) and
// carries node-ness exactly like `WITH n`. A parenthesized
// non-variable keeps the source-text name and no node-ness.

#[test]
fn parenthesized_variables_project_their_name() {
    let item = |expr, alias| paren_item(expr, alias, "(n.p1)");
    // The variable's own name, through any paren depth; an alias wins.
    assert_eq!(
        effective_name(&item(paren_wrap(paren_var("n")), None)),
        Some(SmallName::from_name("n"))
    );
    assert_eq!(
        effective_name(&item(paren_wrap(paren_wrap(paren_var("n"))), None)),
        Some(SmallName::from_name("n"))
    );
    assert_eq!(
        effective_name(&item(
            paren_wrap(paren_var("n")),
            Some(SmallName::from_name("m"))
        )),
        Some(SmallName::from_name("m"))
    );
    // A property access is NOT a canonical shape: Neo4j (probed live
    // 2026.10) names `n.p1`, `n . p1`, `n.` + backtick + `p1` + backtick
    // and `(n.p1)` each by its verbatim source text — so every spelling
    // beyond the plain one is its own column name, and only an
    // exact-text duplicate conflicts (the drift test pins the same
    // agreement with the IR, whose canonical_name drops the property arm
    // with this change).
    assert_eq!(
        effective_name(&item(paren_wrap(paren_prop("n", "p1")), None)),
        Some(SmallName::from_name("(n.p1)"))
    );
    let spaced = paren_item(paren_prop("n", "p1"), None, "n . p1");
    assert_eq!(
        effective_name(&spaced),
        Some(SmallName::from_name("n . p1"))
    );
    // A dynamic key likewise keeps the source text.
    let dynamic = paren_item(paren_subscript("n", paren_var("k")), None, "n[k]");
    assert_eq!(effective_name(&dynamic), Some(SmallName::from_name("n[k]")));
}

/// REL8-1866's front↔IR drift pin: the front's implicit-name vocabulary
/// ([`implicit_name`]) and the IR's [`relate_ir::canonical_name`] — the
/// one the planner's alias gate reads — are two spellings of one fact
/// over two ASTs, with no compiler to keep them aligned. This test
/// lowers the canonical parser shapes and asserts the two agree on
/// every item, so a shape admitted in one but not the other fails here
/// instead of silently splitting WITH's alias acceptance from the
/// planner's naming (the `is_agg_fn` twin-precedent in `crate::agg`).
/// `*` is the one stated asymmetry — the front carries it structurally
/// (the `all` flag, skipped by every alias gate) while the IR names it
/// canonically (`Expr::Star`) — so star items are exempt from the
/// pairwise check.
#[test]
fn implicit_name_agrees_with_ir_canonical_name_on_lowered_shapes() {
    // One statement holding every vocabulary arm and near-miss, each on
    // its own variable so no two items collide on one column name.
    let query = "MATCH (a:L1), (b:L1), (c:L1), (d:L1), (e:L1), (f:L1), (g:L1) \
                 WITH *, a, (b), c{.*}, (d{.*}), e.p1 AS p, (f.p2) AS q, g.p3 + 1 AS r \
                 RETURN a, b{.p4}, c.p5";
    let statement = relate_parser::parse(query).unwrap();
    let mut front_items: Vec<&ReturnItem> = Vec::new();
    for clause in &statement.clauses {
        match clause {
            Clause::With(wc) => front_items.extend(&wc.items),
            Clause::Return(rc) => front_items.extend(&rc.items),
            _ => {}
        }
    }
    let lowered = crate::parse_and_lower(query).unwrap();
    let ir_items: Vec<&relate_ir::Projection> = lowered
        .parts
        .iter()
        .flat_map(|part| &part.clauses)
        .filter_map(|clause| match clause {
            relate_ir::Clause::With { items, .. } | relate_ir::Clause::Return { items, .. } => {
                Some(items)
            }
            _ => None,
        })
        .flatten()
        .collect();
    // Lowering is item-for-item (`lower_projections` maps in order), so
    // the zip is over equal lists — a mismatch is a broken invariant to
    // surface, never a truncated comparison.
    assert_eq!(
        front_items.len(),
        ir_items.len(),
        "lowering must be item-for-item"
    );
    let mut implicit = 0;
    let mut source_text = 0;
    for (front, ir) in front_items.into_iter().zip(ir_items) {
        let label = front.source_text.as_deref().unwrap_or("?");
        if front.all {
            continue;
        }
        let front_implicit = implicit_name(front).is_some();
        assert_eq!(
            front_implicit,
            relate_ir::canonical_name(&ir.expr).is_some(),
            "`{label}`: the front and IR implicit-name vocabularies drifted apart"
        );
        if front_implicit {
            implicit += 1;
        } else {
            source_text += 1;
        }
    }
    // The probe exercises both verdicts — a probe whose shapes stopped
    // parsing (or matching) would pass forever.
    assert_eq!(
        implicit, 6,
        "the implicit arms: a, (b), c{{.*}}, (d{{.*}}), a, b{{.p4}}"
    );
    assert_eq!(
        source_text, 4,
        "the source-text arms: e.p1, (f.p2), g.p3 + 1, c.p5"
    );
}
