//! Vector builtins: thin wrappers over the `eval::vector` helpers, plus the
//! `vector_distance`/`vector_norm` form rows with their typed
//! distance-metric literal.

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::{Expr, VectorMetric};

use crate::overlay::ExecGraph;

use super::super::vector::{
    vector_construct, vector_cosine, vector_dimension_count, vector_distance_metric,
    vector_euclidean, vector_norm_metric,
};
use super::super::{eval, Row};

/// The one metric-literal policy both vector metric forms read: the final
/// argument is a typed `Expr::DistanceMetric` literal — any other
/// expression in that position is the `UnknownDistanceMetric` rejection,
/// checked before any vector operand is evaluated.
fn metric_literal(arg: &Expr) -> Result<VectorMetric> {
    match arg {
        Expr::DistanceMetric(metric) => Ok(*metric),
        _ => Err(Error::syntax("UnknownDistanceMetric")),
    }
}

/// The `vector_distance` form row (CIP-214): two evaluated vectors plus a
/// typed distance-metric literal as the final argument — any other
/// expression in that position is an `UnknownDistanceMetric` error, so
/// the literal check precedes the vector evaluation. The row's
/// exactly-three contract is enforced by the dispatcher before the form
/// runs, so the operands are taken structurally.
pub(super) fn vector_distance(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    args: &[Expr],
) -> Result<Value> {
    let [a, b, metric] =
        <&[Expr; 3]>::try_from(args).expect("the vector_distance row's arity is exactly three");
    let metric = metric_literal(metric)?;
    let vals = [eval(graph, row, a, params)?, eval(graph, row, b, params)?];
    vector_distance_metric(&vals, metric)
}

/// The `vector_norm` form row (CIP-214): the evaluated vector plus its
/// typed distance-metric literal, with the same literal-first shape as
/// [`vector_distance`]. The row's exactly-two contract is enforced by the
/// dispatcher before the form runs, so the operands are taken
/// structurally.
pub(super) fn vector_norm(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    args: &[Expr],
) -> Result<Value> {
    let [vector, metric] =
        <&[Expr; 2]>::try_from(args).expect("the vector_norm row's arity is exactly two");
    let metric = metric_literal(metric)?;
    vector_norm_metric(&eval(graph, row, vector, params)?, metric)
}

pub(super) fn bi_vector(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    vector_construct(&vals)
}

pub(super) fn bi_vector_cosinesimilarity(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    vector_cosine(&vals)
}

pub(super) fn bi_vector_similarity_euclidean(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    vector_euclidean(&vals)
}

pub(super) fn bi_vector_dimension_count(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    vector_dimension_count(&vals)
}
