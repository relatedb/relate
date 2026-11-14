//! The fold-shape builders both the facade's plan-node suite and the
//! executor's candidate-funnel suite build their subjects from (LABS-1083):
//! the disjunction fold's plain-`Or` node and the `var.prop <op> bound`
//! WHERE conjunct, spelled once.

use relate_ir::{BinaryOp, Expr, LabelExpr, NodePattern};
use relate_names::SmallName;

/// The disjunction fold's plain-`Or` node shape: no conjunction labels, the
/// `Or` in the label expression, what `MATCH (p) WHERE p:Model OR p:Source`
/// carries after the rewrite.
pub fn or_node(variable: Option<&str>, disjuncts: &[&str]) -> NodePattern {
    NodePattern {
        variable: variable.map(SmallName::from_name),
        labels: Vec::new(),
        label_expr: Some(LabelExpr::Or(
            disjuncts
                .iter()
                .map(|l| LabelExpr::Label((*l).into()))
                .collect(),
        )),
        properties: None,
        prop_keys: Vec::new(),
        where_clause: None,
        force_scan: false,
    }
}

/// `var.prop <op> bound` as a WHERE conjunct `Expr`.
pub fn comparison(var: &str, prop: &str, op: BinaryOp, bound: Expr) -> Expr {
    Expr::Binary {
        op,
        left: Box::new(Expr::property(
            Expr::Variable(SmallName::from_name(var)),
            SmallName::from_name(prop),
        )),
        right: Box::new(bound),
    }
}
