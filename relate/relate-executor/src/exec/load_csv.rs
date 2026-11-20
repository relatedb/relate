//! The `LOAD CSV` clause: read the source named by each row's URL and bind
//! one row per CSV record.
//!
//! The generator form (ADR-0555, the shared feed spine in `feed.rs`): the
//! source's records are walked one at a time through the record cursor and
//! the rows are handed to the consumer as they are built — never
//! collected — so a consumer that folds rows as they arrive (an
//! aggregation) holds constant state however long the file is, and a
//! consumer that stops early never reads the rest of it. The source's
//! *text* is the reader's memoized `Arc<str>` (one physical read per
//! canonical source, ADR-0081): the clause streams records and rows, not
//! the file read.
//!
//! [`exec_load_csv`] is the materializing form for a consumer that cannot
//! fold as rows arrive, charging each source's output to the running total
//! before its first row is built (ADR-0463).

use std::sync::Arc;

use relate_core::{Error, Result, Value};
use relate_ir::Expr;
use relate_names::SmallName;

use crate::csv::{field_terminator_char, record_value, split_csv_line, RecordCursor};
use crate::ctx::ExecCtx;
use crate::eval::eval;
use crate::row::Row;

use super::feed::{FeedSource, StreamedFeed};

/// The read opened on one input row: the row being read for (the template
/// the bound record column joins), its source's remaining records, and —
/// under `WITH HEADERS`, one fact, not the flag re-encoded — the header
/// names.
struct OpenRead {
    template: Row,
    cursor: RecordCursor<Arc<str>>,
    headers: Option<Vec<String>>,
}

/// The blank-record policy, spelled once: a blank physical record yields
/// no row, so the walk skips it and the charge does not bill it.
fn yields_row(record: &str) -> bool {
    !record.is_empty()
}

/// The `LOAD CSV` producer: the [`FeedSource`] the shared spine drives.
/// Owns the field terminator (resolved once at construction — an invalid
/// one refuses the clause before any file is read), the URL expression,
/// the bound variable, and the read opened on the current input row.
pub(crate) struct LoadCsvSource<'a> {
    delim: char,
    with_headers: bool,
    url: &'a Expr,
    variable: &'a SmallName,
    current: Option<OpenRead>,
}

impl<'a> LoadCsvSource<'a> {
    /// A bad terminator is a clause-level refusal: it fails here, before
    /// the first source is opened.
    pub(crate) fn new(
        with_headers: bool,
        url: &'a Expr,
        variable: &'a SmallName,
        field_terminator: Option<&'a str>,
    ) -> Result<Self> {
        Ok(Self {
            delim: field_terminator_char(field_terminator)?,
            with_headers,
            url,
            variable,
            current: None,
        })
    }
}

impl FeedSource for LoadCsvSource<'_> {
    fn open(&mut self, ctx: ExecCtx<'_>, row: Row) -> Result<bool> {
        let src = eval(ctx.graph, &row, self.url, ctx.params)?;
        let path = match src {
            Value::String(s) => s,
            // A null URL contributes no rows at all: the input row is
            // skipped, not padded. The skip still samples the cancel
            // tick, so a source of only null URLs stays cancellable.
            Value::Null => {
                ctx.graph.cancel_tick()?;
                return Ok(false);
            }
            _ => return Err(Error::ty("LOAD CSV FROM requires a string URL")),
        };
        // The source boundary (`CsvReader::load`) resolves the CIP-202
        // canned fixture URL; everything else goes through the
        // local-file policy (ADR-0081).
        let text: Arc<str> = ctx.graph.csv.load(&path)?;
        let mut cursor = RecordCursor::new(text);
        // The header record is extracted first, so a leading blank line
        // under WITH HEADERS cannot strip the header and flip map rows
        // to list rows.
        let headers: Option<Vec<String>> = if self.with_headers {
            cursor
                .next_record(self.delim)
                .map(|h| split_csv_line(h, self.delim))
        } else {
            None
        };
        self.current = Some(OpenRead {
            template: row,
            cursor,
            headers,
        });
        Ok(true)
    }

    fn charge_facts(&self) -> Result<(usize, usize)> {
        // Read off the open read alone — the materializing form's own
        // fact: the streamed path never counts the records it will not
        // bill (the count re-walks the remaining text). The count is the
        // rows `pull` will build, so a blank record the walk skips is
        // never billed either.
        let current = self
            .current
            .as_ref()
            .expect("charge_facts is read on an open source");
        Ok((
            current.cursor.remaining_records(self.delim, yields_row),
            current.template.len().saturating_add(1),
        ))
    }

    fn pull(&mut self, ctx: ExecCtx<'_>) -> Option<Result<Row>> {
        let current = self.current.as_mut()?;
        loop {
            let record = current.cursor.next_record(self.delim)?;
            // The cancel sample sits at the loop head — before the skip —
            // so a run of blank physical lines stays cancellable.
            if let Err(error) = ctx.graph.cancel_tick() {
                return Some(Err(error));
            }
            // Blank-line policy for the data records: a blank record (an
            // empty physical line between record breaks) yields no row.
            if !yields_row(record) {
                continue;
            }
            // Ragged-row policy (docs/api/cli/spec.md): under WITH HEADERS a
            // record narrower than the header maps its missing names to
            // null — Neo4j's `LOAD CSV WITH HEADERS` semantics, applied at
            // the parse boundary by `record_value`'s named rule
            // (`zip_with_header_nulls`); extra cells are dropped. List rows
            // keep their natural width.
            let val = record_value(
                current.headers.as_deref(),
                split_csv_line(record, self.delim),
            );
            let mut row = current.template.clone();
            row.insert(self.variable.to_string(), val);
            return Some(Ok(row));
        }
    }
}

/// The clause's materializing form: the feed spine's `materialize` for a
/// consumer that cannot fold rows as they arrive (`exec_unwind`'s shape).
pub(crate) fn exec_load_csv(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    with_headers: bool,
    url: &Expr,
    variable: &SmallName,
    field_terminator: Option<&str>,
) -> Result<Vec<Row>> {
    load_csv_feed(
        ctx,
        rows,
        with_headers,
        url,
        variable,
        field_terminator,
        false,
    )?
    .materialize()
}

/// The clause's feed over its source: the constructor both the parked
/// (fused) and the materializing form build, so the two share the source
/// construction and its terminator refusal.
pub(crate) fn load_csv_feed<'a>(
    ctx: ExecCtx<'a>,
    rows: Vec<Row>,
    with_headers: bool,
    url: &'a Expr,
    variable: &'a SmallName,
    field_terminator: Option<&'a str>,
    profiled: bool,
) -> Result<StreamedFeed<'a>> {
    Ok(StreamedFeed::new(
        ctx,
        rows,
        Box::new(LoadCsvSource::new(
            with_headers,
            url,
            variable,
            field_terminator,
        )?),
        profiled,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use relate_core::{CapacityKind, Error, Value};
    use relate_ir::Expr;
    use relate_names::SmallName;

    use super::super::test_support::{exec_cypher_on, profiled_write_routed, try_exec_cypher_on};
    use super::{exec_load_csv, LoadCsvSource};
    use crate::csv::{local_file_url, test_root, CsvPolicy};
    use crate::ctx::ExecCtx;
    use crate::exec::feed::FeedSource;
    use crate::row::Row;
    use crate::{ExecGraph, Graph};

    fn csv_rooted_graph(root: &std::path::Path) -> Graph {
        let mut graph = Graph::default();
        graph.configure_load_csv(CsvPolicy::new(Some(root.to_path_buf()), None).unwrap());
        graph
    }

    fn csv_row(value: &str) -> Vec<Value> {
        vec![Value::List(vec![Value::String(value.into())].into())]
    }

    #[test]
    fn load_csv_rejects_multi_char_field_terminator() {
        let graph = Graph::default();
        let view = ExecGraph::read(&graph);
        let params = BTreeMap::new();
        let url = Expr::String("rows.csv".into());
        let err = exec_load_csv(
            ExecCtx::new(&view, &params),
            vec![Row::new()],
            false,
            &url,
            &SmallName::from_name("row"),
            Some("||"),
        )
        .unwrap_err();
        assert_eq!(
            err,
            Error::ty("LOAD CSV FIELDTERMINATOR must be exactly one character")
        );
    }

    #[test]
    fn one_physical_read_per_statement() {
        let dir = test_root();
        let root = dir.path();
        fs::write(root.join("rows.csv"), "a\nb\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("rows.csv", root.join("alias.csv")).unwrap();
        let graph = csv_rooted_graph(root);
        let view = ExecGraph::read(&graph);
        let result = exec_cypher_on(
            &view,
            "UNWIND [1,2,3] AS n LOAD CSV FROM 'rows.csv' AS row RETURN n, row",
        );
        assert_eq!(result.rows.len(), 6);
        assert_eq!(
            result.rows[0],
            vec![
                Value::Int(1),
                Value::List(vec![Value::String("a".into())].into())
            ]
        );
        assert_eq!(view.csv.read_count(), 1);
        #[cfg(unix)]
        assert_eq!(view.csv.load("alias.csv").unwrap().as_ref(), "a\nb\n");
        assert_eq!(view.csv.read_count(), 1);
        assert_eq!(
            ExecGraph::read(&graph)
                .csv
                .load("rows.csv")
                .unwrap()
                .as_ref(),
            "a\nb\n"
        );
        assert_eq!(view.csv.read_count(), 1);
        // The CIP-202 fixture resolves from canned data, never a physical
        // read, even with a local root configured.
        assert_eq!(
            view.csv
                .load("https://data.neo4j.com/bands/artists.csv")
                .unwrap()
                .as_ref(),
            "1,ABBA,1992\n2,Roxette,1986\n3,Europe,1979\n"
        );
        assert_eq!(view.csv.read_count(), 1);
    }

    #[test]
    fn load_csv_honors_rfc4180_quoting_across_lines() {
        let dir = test_root();
        let root = dir.path();
        fs::write(
            root.join("rows.csv"),
            "id,title,owner\n1,\"Neo4j, graph\nand \"\"Cypher\"\"\",42\n2,plain,7\n",
        )
        .unwrap();
        let graph = csv_rooted_graph(root);
        let view = ExecGraph::read(&graph);
        let result = exec_cypher_on(
            &view,
            "LOAD CSV WITH HEADERS FROM 'rows.csv' AS r RETURN r.id, r.title, r.owner",
        );
        let row = |id: &str, title: &str, owner: &str| {
            vec![id, title, owner]
                .into_iter()
                .map(|s| Value::String(s.into()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            result.rows,
            vec![
                row("1", "Neo4j, graph\nand \"Cypher\"", "42"),
                row("2", "plain", "7"),
            ]
        );
    }

    /// Neo4j-parity ragged-row rule: under WITH HEADERS a short record maps
    /// the missing names to null (an absent field is distinguishable from
    /// an empty one); extra cells past the header width are dropped; list
    /// rows keep their natural width.
    #[test]
    fn load_csv_with_headers_maps_missing_cells_to_null() {
        let dir = test_root();
        let root = dir.path();
        fs::write(root.join("ragged.csv"), "a,b,c\n1,2\n3,4,5,6\n9,\n").unwrap();
        let graph = csv_rooted_graph(root);
        let view = ExecGraph::read(&graph);
        let result = exec_cypher_on(
            &view,
            "LOAD CSV WITH HEADERS FROM 'ragged.csv' AS r RETURN r.a, r.b, r.c",
        );
        let s = |v: &str| Value::String(v.into());
        assert_eq!(
            result.rows,
            vec![
                vec![s("1"), s("2"), Value::Null],
                vec![s("3"), s("4"), s("5")],
                vec![s("9"), s(""), Value::Null],
            ]
        );
        let lists = exec_cypher_on(&view, "LOAD CSV FROM 'ragged.csv' AS row RETURN row");
        assert_eq!(
            lists.rows,
            vec![
                vec![Value::List(vec![s("a"), s("b"), s("c")].into())],
                vec![Value::List(vec![s("1"), s("2")].into())],
                vec![Value::List(vec![s("3"), s("4"), s("5"), s("6")].into())],
                vec![Value::List(vec![s("9"), s("")].into())],
            ]
        );
    }

    #[test]
    fn header_record_is_extracted_before_the_blank_line_policy() {
        let dir = test_root();
        let root = dir.path();
        // A blank line after the header yields no row; the header is the
        // file's first record.
        fs::write(root.join("rows.csv"), "id,name\n\n1,abba\n").unwrap();
        let graph = csv_rooted_graph(root);
        let view = ExecGraph::read(&graph);
        let result = exec_cypher_on(
            &view,
            "LOAD CSV WITH HEADERS FROM 'rows.csv' AS r RETURN r.id, r.name",
        );
        assert_eq!(
            result.rows,
            vec![vec![
                Value::String("1".into()),
                Value::String("abba".into()),
            ]]
        );
        // A leading blank line is the header record (one empty name): the
        // blank-line policy never runs ahead of header extraction, so map
        // rows cannot silently degrade to list rows.
        fs::write(root.join("leading.csv"), "\n1,2\n").unwrap();
        let result = exec_cypher_on(
            &view,
            "LOAD CSV WITH HEADERS FROM 'leading.csv' AS r RETURN keys(r), r['']",
        );
        assert_eq!(
            result.rows,
            vec![vec![
                Value::List(vec![Value::String("".into())].into()),
                Value::String("1".into()),
            ]]
        );
    }

    #[test]
    fn reused_view_reads_edited_source_on_each_execute() {
        let dir = test_root();
        let root = dir.path();
        let path = root.join("rows.csv");
        fs::write(&path, "first\n").unwrap();
        let graph = csv_rooted_graph(root);
        let view = ExecGraph::read(&graph);
        let query = "UNWIND [1,2,3] AS n LOAD CSV FROM 'rows.csv' AS row RETURN row";

        assert_eq!(exec_cypher_on(&view, query).rows, vec![csv_row("first"); 3]);
        assert_eq!(view.csv.read_count(), 1);
        fs::write(&path, "second\n").unwrap();
        assert_eq!(
            exec_cypher_on(&view, query).rows,
            vec![csv_row("second"); 3]
        );
        assert_eq!(view.csv.read_count(), 2);
    }

    #[test]
    fn reused_view_clears_canonical_memo_for_distinct_source_spellings() {
        let dir = test_root();
        let root = dir.path();
        let path = root.join("rows.csv");
        fs::write(&path, "first\n").unwrap();
        let graph = csv_rooted_graph(root);
        let view = ExecGraph::read(&graph);

        let relative = "LOAD CSV FROM 'rows.csv' AS row RETURN row";
        assert_eq!(exec_cypher_on(&view, relative).rows, vec![csv_row("first")]);
        fs::write(&path, "second\n").unwrap();
        let absolute = format!(
            "LOAD CSV FROM '{}' AS row RETURN row",
            local_file_url(&path)
        );
        assert_eq!(
            exec_cypher_on(&view, &absolute).rows,
            vec![csv_row("second")]
        );
        assert_eq!(view.csv.read_count(), 2);
    }

    #[test]
    fn reused_view_reports_removed_source_on_next_execute() {
        let dir = test_root();
        let root = dir.path();
        let path = root.join("rows.csv");
        fs::write(&path, "first\n").unwrap();
        let graph = csv_rooted_graph(root);
        let view = ExecGraph::read(&graph);
        let query = "LOAD CSV FROM 'rows.csv' AS row RETURN row";

        assert_eq!(exec_cypher_on(&view, query).rows, vec![csv_row("first")]);
        fs::remove_file(path).unwrap();
        assert_eq!(
            try_exec_cypher_on(&view, query).err().unwrap(),
            Error::execution("LOAD CSV file unavailable")
        );
        assert_eq!(view.csv.read_count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn reused_view_observes_in_root_symlink_retarget_next_execute() {
        use std::os::unix::fs::symlink;

        let dir = test_root();
        let root = dir.path();
        fs::write(root.join("first.csv"), "first\n").unwrap();
        fs::write(root.join("second.csv"), "second\n").unwrap();
        let alias = root.join("alias.csv");
        symlink("first.csv", &alias).unwrap();
        let graph = csv_rooted_graph(root);
        let view = ExecGraph::read(&graph);
        let first =
            "UNWIND ['alias.csv','first.csv','alias.csv'] AS src LOAD CSV FROM src AS row RETURN row";
        assert_eq!(exec_cypher_on(&view, first).rows, vec![csv_row("first"); 3]);
        assert_eq!(view.csv.read_count(), 1);

        fs::remove_file(&alias).unwrap();
        symlink("second.csv", &alias).unwrap();
        let second = "UNWIND ['alias.csv','second.csv'] AS src LOAD CSV FROM src AS row RETURN row";
        assert_eq!(
            exec_cypher_on(&view, second).rows,
            vec![csv_row("second"); 2]
        );
        assert_eq!(view.csv.read_count(), 2);
    }

    /// LABS-1845: the materializing form charges each source's output
    /// (records x carried width) to the running total before its first row
    /// is built — the wide-row `UNWIND` charge rule, same numbers,
    /// different producer. A file a plain (non-folding) projection
    /// consumes must refuse at a lowered cap instead of building the rows.
    #[test]
    fn loading_a_plain_projection_charges_the_row_clones() {
        let dir = test_root();
        let root = dir.path();
        fs::write(root.join("many.csv"), "1\n".repeat(50_000)).unwrap();
        let mut graph = csv_rooted_graph(root);
        graph.configure_expression_memory_limit(Some(1 << 20));
        let view = ExecGraph::read(&graph);
        let error = try_exec_cypher_on(&view, "LOAD CSV FROM 'many.csv' AS row RETURN row")
            .err()
            .expect("a materialized LOAD CSV must be refused at the lowered cap");
        assert!(
            matches!(
                &error,
                Error::Capacity {
                    kind: CapacityKind::Expression,
                    ..
                }
            ),
            "expected the typed expression-capacity refusal, got: {error}"
        );
    }

    /// LABS-1845 (ADR-0555): the same file, the same lowered cap, the same
    /// clause — only the consumer differs. An aggregation folds the rows as
    /// the records are read, so the charge the plain projection refuses is
    /// never booked and the answer is one integer however long the file is:
    /// the parked feed never materializes (the distinguishing input is the
    /// consumer, the file is identical to the refusing test above).
    #[test]
    fn a_folding_consumer_streams_the_file_whatever_the_record_count() {
        let dir = test_root();
        let root = dir.path();
        fs::write(root.join("many.csv"), "1\n".repeat(50_000)).unwrap();
        let mut graph = csv_rooted_graph(root);
        graph.configure_expression_memory_limit(Some(1 << 20));
        let view = ExecGraph::read(&graph);
        let result = exec_cypher_on(
            &view,
            "LOAD CSV FROM 'many.csv' AS row RETURN count(row) AS c",
        );
        assert_eq!(result.rows, vec![vec![Value::Int(50_000)]]);
    }

    /// LABS-1845: a streamed `LOAD CSV` reports its own clause facts after
    /// its consumer drained it — the rows it generated (3, the file's
    /// records), one row's size as its in-flight memory, no expression
    /// bytes (no list was built) — at its own PROFILE position, while the
    /// consuming aggregation reports its own.
    #[test]
    fn a_streamed_load_csv_records_its_own_clause_facts() {
        let dir = test_root();
        let root = dir.path();
        fs::write(root.join("rows.csv"), "a\nb\nc\n").unwrap();
        let graph = csv_rooted_graph(root);
        let records = profiled_write_routed(
            &graph,
            "LOAD CSV FROM 'rows.csv' AS row RETURN count(row) AS c",
        );
        assert_eq!(
            records.len(),
            2,
            "the LOAD CSV and its consumer, one record each"
        );
        let load = &records[0];
        assert_eq!(load.clause, 0);
        assert_eq!(
            load.rows, 3,
            "the parked feed's facts report after the drain"
        );
        assert_eq!(load.expression_built_bytes, 0, "no list was built");
        assert!(
            load.memory_bytes > 0,
            "one row's size is the in-flight memory"
        );
        assert_eq!(records[1].clause, 1);
    }

    /// LABS-1845: the charge facts are the materializing form's alone — a
    /// source opened for streaming never counts its records. The count
    /// (which re-walks the remaining text) is read off the open read only
    /// by `charge_facts`, and it bills the rows the walk will build: the
    /// seam's own contract is pinned here — open, then count, then pull —
    /// with a blank record in the file, which yields no row and so is not
    /// billed.
    #[test]
    fn the_charge_count_is_the_rows_the_open_source_will_add() {
        let dir = test_root();
        let root = dir.path();
        fs::write(root.join("rows.csv"), "a\n\nb\nc\n").unwrap();
        let graph = csv_rooted_graph(root);
        let view = ExecGraph::read(&graph);
        let params = BTreeMap::new();
        let url = Expr::String("rows.csv".into());
        let variable = SmallName::from_name("row");
        let mut source = LoadCsvSource::new(false, &url, &variable, None).unwrap();
        let ctx = ExecCtx::new(&view, &params);
        // Nothing open yet: the spine never reads the facts before an
        // open succeeds, and pulling with nothing open is spent.
        assert!(source.pull(ctx).is_none());
        assert!(source.open(ctx, Row::new()).unwrap());
        assert_eq!(
            source.charge_facts().unwrap().0,
            3,
            "three records yield a row; the blank one is not billed"
        );
        let mut pulled = 0;
        while let Some(row) = source.pull(ctx) {
            row.unwrap();
            pulled += 1;
        }
        assert_eq!(pulled, 3, "the count matches what the walk built");
    }
}
