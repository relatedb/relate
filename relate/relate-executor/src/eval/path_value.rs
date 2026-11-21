//! `PATH [n1, r1, n2, ...]` construction and `p1 || p2` concatenation.

use relate_core::{Error, Path, Result, Value};

use crate::overlay::ExecGraph;

/// Builds a path from evaluated constructor elements. Any null element
/// yields null; the list must alternate node, relationship, ... starting
/// and ending with a node, and each relationship must join its neighbours
/// in either stored direction.
pub(super) fn construct(graph: &ExecGraph<'_>, elements: Vec<Value>) -> Result<Value> {
    if elements.iter().any(|v| matches!(v, Value::Null)) {
        return Ok(Value::Null);
    }
    if elements.len() % 2 == 0 {
        return Err(Error::invalid_argument_type(
            "PATH [ ... ] requires an alternating node, relationship, ..., node list",
        ));
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
            _ => {
                return Err(Error::invalid_argument_type(
                    "PATH [ ... ] requires an alternating node, relationship, ..., node list",
                ))
            }
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

    fn construct(items: Vec<Value>) -> relate_core::Result<Value> {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let expr = Expr::PathConstruct {
            elements: items.into_iter().map(Expr::RuntimeValue).collect(),
        };
        super::super::eval(&graph, &Row::new(), &expr, &BTreeMap::new())
    }

    fn concat(l: Value, r: Value, on: bool) -> relate_core::Result<Value> {
        let mut g = Graph::default();
        if on {
            g.configure_preview(
                PreviewFeatures::none().with_feature(PreviewFeature::PathConcatenation),
            );
        }
        let graph = ExecGraph::read(&g);
        let expr = Expr::Binary {
            op: BinaryOp::Concat,
            left: Box::new(Expr::RuntimeValue(l)),
            right: Box::new(Expr::RuntimeValue(r)),
        };
        super::super::eval(&graph, &Row::new(), &expr, &BTreeMap::new())
    }

    fn path_of(v: Value) -> Arc<relate_core::Path> {
        match v {
            Value::Path(p) => p,
            _ => panic!("expected a path value"),
        }
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
        let mut g = Graph::default();
        g.configure_expression_memory_limit(Some(1));
        g.configure_preview(
            PreviewFeatures::none().with_feature(PreviewFeature::PathConcatenation),
        );
        let graph = ExecGraph::read(&g);
        let p = path_of(construct(vec![node(1), rel(9, 1, 2), node(2)]).unwrap());
        let items = [node(1), rel(9, 1, 2), node(2)]
            .map(Expr::RuntimeValue)
            .to_vec();
        let err = super::super::eval(
            &graph,
            &Row::new(),
            &Expr::PathConstruct { elements: items },
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Capacity { .. }), "{err}");
        let q = path_of(construct(vec![node(2), rel(8, 2, 3), node(3)]).unwrap());
        let err = super::join(&graph, &p, &q).unwrap_err();
        assert!(matches!(err, Error::Capacity { .. }), "{err}");
    }

    #[test]
    fn path_concat_joins_at_the_shared_node() {
        let a = construct(vec![node(1), rel(9, 1, 2), node(2)]).unwrap();
        let b = construct(vec![node(2), rel(8, 2, 3), node(3)]).unwrap();
        let joined = path_of(concat(a.clone(), b.clone(), true).unwrap());
        assert_eq!((joined.nodes.len(), joined.rels.len()), (3, 2));
        assert!(matches!(
            concat(b.clone(), a.clone(), true),
            Err(Error::InvalidArgumentType(_))
        ));
        assert_eq!(concat(a.clone(), Value::Null, true).unwrap(), Value::Null);
        assert!(concat(a.clone(), Value::Int(1), true).is_err());
        let off = concat(a, b, false).unwrap_err();
        assert!(off
            .to_string()
            .ends_with("|| requires two strings or two lists"));
    }
}
