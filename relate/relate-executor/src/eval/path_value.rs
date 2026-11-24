//! `PATH [n1, r1, n2, ...]` construction and `p1 || p2` concatenation.

use relate_core::{Error, Path, Result, Value};

use crate::overlay::ExecGraph;

/// The one rejection for an element list that does not alternate node,
/// relationship, ..., node.
const ALTERNATING_LIST: &str =
    "PATH [ ... ] requires an alternating node, relationship, ..., node list";

/// Builds a path from evaluated constructor elements. Any null element
/// yields null; the list must alternate node, relationship, ... starting
/// and ending with a node, and each relationship must join its neighbours
/// in either stored direction.
pub(super) fn construct(graph: &ExecGraph<'_>, elements: Vec<Value>) -> Result<Value> {
    if elements.iter().any(|v| matches!(v, Value::Null)) {
        return Ok(Value::Null);
    }
    if elements.len().is_multiple_of(2) {
        return Err(Error::invalid_argument_type(ALTERNATING_LIST));
    }
    // ADR-0463: charge the element vectors before building them.
    graph.charge_value_buffer(elements.len() / 2 + 1)?;
    graph.charge_value_buffer(elements.len() / 2)?;
    let mut path = Path {
        nodes: Vec::new(),
        rels: Vec::new(),
    };
    for (i, element) in elements.into_iter().enumerate() {
        match (i % 2, element) {
            (0, Value::Node(n)) => path.nodes.push((*n).clone()),
            (1, Value::Relationship(r)) => path.rels.push((*r).clone()),
            _ => return Err(Error::invalid_argument_type(ALTERNATING_LIST)),
        }
    }
    for (rel, pair) in path.rels.iter().zip(path.nodes.windows(2)) {
        let (a, b) = (pair[0].id, pair[1].id);
        if !((rel.start == a && rel.end == b) || (rel.start == b && rel.end == a)) {
            return Err(Error::invalid_argument_type(
                "PATH [ ... ] relationship does not connect its neighbouring nodes",
            ));
        }
    }
    Ok(Value::from_path(path))
}

/// Joins two paths at their shared boundary node (same node id).
pub(super) fn join(graph: &ExecGraph<'_>, a: &Path, b: &Path) -> Result<Value> {
    // `Path` is a public struct: a path parameter can arrive with no nodes,
    // so a missing boundary node is the same typed rejection as a mismatch.
    let boundary_matches = match (a.nodes.last(), b.nodes.first()) {
        (Some(last), Some(first)) => last.id == first.id,
        _ => false,
    };
    if !boundary_matches {
        return Err(Error::invalid_argument_type(
            "|| requires the last node of the left path to be the first node of the right path",
        ));
    }
    graph.charge_value_buffer(a.nodes.len() + b.nodes.len() - 1)?;
    graph.charge_value_buffer(a.rels.len() + b.rels.len())?;
    let mut nodes = a.nodes.clone();
    nodes.extend(b.nodes.iter().skip(1).cloned());
    let mut rels = a.rels.clone();
    rels.extend(b.rels.iter().cloned());
    Ok(Value::from_path(Path { nodes, rels }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use relate_core::{Error, Node, PreviewFeature, PreviewFeatures, Relationship, Value};
    use relate_ir::{BinaryOp, Expr};

    use crate::exec::test_support::path_value;
    use crate::graph::Graph;
    use crate::overlay::ExecGraph;
    use crate::row::Row;

    fn node(id: i64) -> Value {
        Value::Node(Arc::new(Node {
            id,
            labels: Default::default(),
            properties: Default::default(),
        }))
    }

    fn rel(id: i64, start: i64, end: i64) -> Value {
        Value::Relationship(Arc::new(Relationship {
            id,
            start,
            end,
            rel_type: "R".into(),
            properties: Default::default(),
        }))
    }

    /// A graph with the given preview set configured: the one fixture every
    /// evaluation below runs against.
    fn graph_with(preview: PreviewFeatures) -> Graph {
        let mut g = Graph::default();
        g.configure_preview(preview);
        g
    }

    fn eval_on(g: &Graph, expr: &Expr) -> relate_core::Result<Value> {
        super::super::eval(&ExecGraph::read(g), &Row::new(), expr, &BTreeMap::new())
    }

    fn construct_expr(items: Vec<Value>) -> Expr {
        Expr::PathConstruct {
            elements: items.into_iter().map(Expr::RuntimeValue).collect(),
        }
    }

    fn construct(items: Vec<Value>) -> relate_core::Result<Value> {
        eval_on(&Graph::default(), &construct_expr(items))
    }

    fn concat(l: Value, r: Value, preview: PreviewFeatures) -> relate_core::Result<Value> {
        let expr = Expr::Binary {
            op: BinaryOp::Concat,
            left: Box::new(Expr::RuntimeValue(l)),
            right: Box::new(Expr::RuntimeValue(r)),
        };
        eval_on(&graph_with(preview), &expr)
    }

    fn concat_on() -> PreviewFeatures {
        PreviewFeatures::none().with_feature(PreviewFeature::PathConcatenation)
    }

    fn path_of(v: Value) -> Arc<relate_core::Path> {
        path_value(&v).expect("expected a path value")
    }

    #[test]
    fn path_construct_builds_and_validates() {
        let p = path_of(construct(vec![node(1)]).unwrap());
        assert_eq!((p.nodes.len(), p.rels.len()), (1, 0));
        // Either stored direction is accepted.
        let p = path_of(construct(vec![node(1), rel(9, 2, 1), node(2)]).unwrap());
        assert_eq!((p.nodes.len(), p.rels.len()), (2, 1));
        assert_eq!(
            construct(vec![node(1), Value::Null, node(2)]).unwrap(),
            Value::Null
        );
        for bad in [
            vec![],
            vec![node(1), rel(9, 1, 2)],
            vec![node(1), node(2), node(3)],
            vec![node(1), rel(9, 1, 3), node(2)],
        ] {
            assert!(matches!(construct(bad), Err(Error::InvalidArgumentType(_))));
        }
    }

    #[test]
    fn path_construct_and_join_over_the_cap_error_as_capacity() {
        let mut g = graph_with(concat_on());
        g.configure_expression_memory_limit(Some(1));
        let graph = ExecGraph::read(&g);
        let p = path_of(construct(vec![node(1), rel(9, 1, 2), node(2)]).unwrap());
        let err = eval_on(&g, &construct_expr(vec![node(1), rel(9, 1, 2), node(2)])).unwrap_err();
        assert!(matches!(err, Error::Capacity { .. }), "{err}");
        let q = path_of(construct(vec![node(2), rel(8, 2, 3), node(3)]).unwrap());
        let err = super::join(&graph, &p, &q).unwrap_err();
        assert!(matches!(err, Error::Capacity { .. }), "{err}");
    }

    #[test]
    fn path_concat_with_an_empty_path_parameter_is_a_typed_error() {
        let empty = Value::from_path(relate_core::Path {
            nodes: Vec::new(),
            rels: Vec::new(),
        });
        let a = construct(vec![node(1), rel(9, 1, 2), node(2)]).unwrap();
        for (l, r) in [
            (empty.clone(), a.clone()),
            (a.clone(), empty.clone()),
            (empty.clone(), empty.clone()),
        ] {
            assert!(matches!(
                concat(l, r, concat_on()),
                Err(Error::InvalidArgumentType(_))
            ));
        }
    }

    #[test]
    fn path_concat_joins_at_the_shared_node() {
        let a = construct(vec![node(1), rel(9, 1, 2), node(2)]).unwrap();
        let b = construct(vec![node(2), rel(8, 2, 3), node(3)]).unwrap();
        let joined = path_of(concat(a.clone(), b.clone(), concat_on()).unwrap());
        assert_eq!((joined.nodes.len(), joined.rels.len()), (3, 2));
        assert!(matches!(
            concat(b.clone(), a.clone(), concat_on()),
            Err(Error::InvalidArgumentType(_))
        ));
        assert_eq!(
            concat(a.clone(), Value::Null, concat_on()).unwrap(),
            Value::Null
        );
        assert!(concat(a.clone(), Value::Int(1), concat_on()).is_err());
        let off = concat(a, b, PreviewFeatures::none()).unwrap_err();
        assert!(off
            .to_string()
            .ends_with("|| requires two strings or two lists"));
    }
}
