//! Result readers: column and cell extractors for the exec tests.
use std::sync::Arc;

use relate_core::{NodeId, RelId, Value};

use super::super::QueryResult;

/// The cells of result column `name`, in result order, extracted through
/// `value`. The one column reader for the exec tests: a missing column or a
/// cell of the wrong variant panics with the column name.
pub(crate) fn column<T>(
    result: &QueryResult,
    name: &str,
    value: impl Fn(&Value) -> Option<T>,
) -> Vec<T> {
    let index = result
        .columns
        .iter()
        .position(|column| column == name)
        .unwrap_or_else(|| panic!("no `{name}` column in {:?}", result.columns));
    result
        .rows
        .iter()
        .map(|row| {
            value(&row[index]).unwrap_or_else(|| {
                panic!("`{name}` cell is not the expected type: {:?}", row[index])
            })
        })
        .collect()
}

/// The STRING cell of a result column: `Some` for a `Value::String`
/// cell, `None` for any other variant — the one string-cell reader the
/// exec tests pass to [`column`] (the SHOW `createStatement` reads, the
/// batch tests' id columns), never a second closure spelling. The
/// variant discrimination itself is core's canonical
/// [`Value::as_string`](relate_core::Value::as_string) accessor,
/// the same one the graph layer's research kernels read through.
pub(crate) fn string_cell(value: &Value) -> Option<String> {
    value.as_string().map(str::to_string)
}

/// The list cell of a result column: `Some` for a [`Value::List`] cell,
/// `None` for any other variant — the list-column reader beside
/// [`string_cell`] and [`int_cell`], so no test spells the extraction
/// conditional inline.
pub(crate) fn list_cell(value: &Value) -> Option<Value> {
    match value {
        Value::List(_) => Some(value.clone()),
        _ => None,
    }
}

/// The integer cell of a value: `Some` for [`Value::Int`], `None` for any
/// other variant — the int-column reader beside [`string_cell`], so no
/// test spells the extraction conditional inline. Stricter than
/// [`Value::as_int`], which coerces whole-number floats: a result column
/// reads back exactly the type that was written, so a `Float` cell
/// reaching an int assertion should fail, not coerce silently.
pub(crate) fn int_cell(value: &Value) -> Option<i64> {
    match value {
        Value::Int(value) => Some(*value),
        _ => None,
    }
}

/// The node id of a `RETURN n` cell (one definition for every test that
/// reads a node back out of a result row).
pub(crate) fn node_id(value: &Value) -> NodeId {
    let Value::Node(n) = value else {
        panic!("expected a node, got {value:?}");
    };
    n.id
}

/// The relationship id of a `RETURN r` cell (the `node_id` twin, same one
/// definition for every test).
pub(crate) fn rel_id(value: &Value) -> RelId {
    let Value::Relationship(r) = value else {
        panic!("expected a relationship, got {value:?}");
    };
    r.id
}

/// The path value of a `RETURN p` cell — the `node_id`/`rel_id` family's
/// path member, in the shape its consumer reads: `None` on a non-path
/// cell, so `column`'s own panic (which names the column) stays the one
/// spelling for a wrong-variant cell; the twins keep their direct-cell
/// panics because their callers read cells by row, not by column.
pub(crate) fn path_value(value: &Value) -> Option<Arc<relate_core::Path>> {
    let Value::Path(p) = value else {
        return None;
    };
    Some(Arc::clone(p))
}
