//! The `LOAD CSV` clause: read the source named by each row's URL and bind
//! one row per CSV record.

use std::sync::Arc;

use relate_core::{Error, Result, Value};
use relate_ir::Expr;
use relate_names::SmallName;

use crate::csv::{field_terminator_char, record_value, split_csv_line, split_csv_records};
use crate::ctx::ExecCtx;
use crate::eval::eval;
use crate::row::Row;

pub(crate) fn exec_load_csv(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    with_headers: bool,
    url: &Expr,
    variable: &SmallName,
    field_terminator: Option<&str>,
) -> Result<Vec<Row>> {
    let ExecCtx { graph, params, .. } = ctx;
    let delim = field_terminator_char(field_terminator)?;
    let mut out = Vec::new();
    for row in rows {
        let src = eval(graph, &row, url, params)?;
        let path = match src {
            Value::String(s) => s,
            Value::Null => continue,
            _ => return Err(Error::ty("LOAD CSV FROM requires a string URL")),
        };
        // The source boundary (`CsvReader::load`) resolves the CIP-202
        // canned fixture URL; everything else goes through the local-file
        // policy (ADR-0081).
        let text: Arc<str> = graph.csv.load(&path)?;
        let mut records = split_csv_records(&text, delim).into_iter();
        // The header record is extracted first, so a leading blank line
        // under WITH HEADERS cannot strip the header and flip map rows to
        // list rows.
        let headers: Option<Vec<String>> = if with_headers {
            records.next().map(|h| split_csv_line(h, delim))
        } else {
            None
        };
        // Blank-line policy for the data records: a blank record (an empty
        // physical line between record breaks) yields no row.
        let records = records.filter(|r| !r.is_empty());
        for record in records {
            // Ragged-row policy (docs/api/cli/spec.md): under WITH HEADERS a
            // record narrower than the header maps its missing names to
            // null — Neo4j's `LOAD CSV WITH HEADERS` semantics, a
            // deliberate behavior change from the earlier empty-string
            // padding, applied at the parse boundary by `record_value`'s
            // named rule (`zip_with_header_nulls`); extra cells are
            // dropped. List rows keep their natural width.
            let val = record_value(headers.as_deref(), split_csv_line(record, delim));
            let mut next = row.clone();
            next.insert(variable.to_string(), val);
            out.push(next);
        }
    }
    Ok(out)
}
