//! The diverted scan calls — the vector and fulltext `db.index.*.query*`
//! procedures: their composition sources, the one construction both the
//! parked (streamed, ADR-0555) and the materializing (charged, ADR-0463)
//! form share. [`scan_call_source`] owns the whole shared skeleton — the
//! contract checked over the classification's own signature, the per-row
//! index-name read, the (entity column, score) row mapping — and each
//! diverted scan supplies only its per-row candidate search; the scans
//! themselves live in the sibling `fulltext` and `vector` modules.

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::Expr;
use relate_model::EntityKind;

use crate::ctx::ExecCtx;
use crate::eval::eval;
use crate::exec::candidates::ranked_candidates;
use crate::procedure_registry::ProcSignature;
use crate::row::Row;

use super::super::compose::{ComposeSource, Produced};
use super::super::fulltext;
use super::super::vector::{resolve_vector_index, scan_candidates, vector_operand};
use super::contract::CallSpec;

/// The entity-kind mismatch rejection both scan bodies call —
/// `queryNodes` on a relationship index (or the reverse) has no scan to
/// run. The family word names the scan's own index kind.
fn entity_kind_mismatch(kind: EntityKind, resolved: EntityKind, family: &str) -> Result<()> {
    if resolved != kind {
        return Err(Error::execution(format!(
            "WrongIndexType: {family} index entity type mismatch"
        )));
    }
    Ok(())
}

/// The facts one scan body reads per input row: the classified kind, the
/// validated positional args, the execution context, and the row's
/// resolved index name — the context whole, never re-split into a
/// graph/params pair beside the one the caller already holds.
struct ScanInputs<'a> {
    kind: EntityKind,
    args: &'a [Expr],
    ctx: ExecCtx<'a>,
    index_name: String,
}

/// The one scan-call composition source both diverted scans build: the
/// contract-checked preamble, the per-row index-name read, and the
/// (entity column, score) row mapping over the candidates the scan body
/// answers — a folding consumer drains it row by row (ADR-0555); a plain
/// projection materializes it with the ADR-0463 charge.
fn scan_call_source<'a>(
    ctx: ExecCtx<'a>,
    kind: EntityKind,
    call: CallSpec<'a>,
    signature: &'static ProcSignature,
    mut scan: impl FnMut(&Row, &ScanInputs<'_>) -> Result<Vec<(Value, f64)>> + 'a,
) -> Result<ComposeSource<'a>> {
    let CallSpec {
        yield_items,
        optional,
        where_clause,
        ..
    } = call;
    // The one contract check every dispatch path reads, over the
    // classification's own signature (the passing-mode backstop plus the
    // arity, in the positional form the scans read — a paren-less call
    // passes no list, so it answers the arity rejection exactly like a
    // short explicit one).
    let args = call.check_diverted_positional(signature)?;
    let CallSpec { name, source, .. } = call;
    // The scan's declared return columns are the default yields — the
    // signature table is the one source every path consults (the generic
    // path reads its metadata's yield names from the same table), and the
    // produced cells are keyed by the same columns, so the declared and
    // produced spellings cannot drift.
    let columns: Vec<&'static str> = signature.returns.iter().map(|ret| ret.name).collect();
    // The defaults handed to the composition's YIELD resolution; the same
    // declared columns, cloned into the producer for the produced cells'
    // keys (two 'static slices — pointer copies, never per-row work).
    let defaults = columns.clone();
    Ok(ComposeSource::new(
        &defaults,
        yield_items,
        optional,
        where_clause,
        move |input| {
            // The first positional argument — the one typed required-STRING
            // reader every CALL path shares.
            let index_name = super::contract::string_arg(ctx, input, source, 0, "indexName", name)?;
            let inputs = ScanInputs {
                kind,
                args,
                ctx,
                index_name,
            };
            Ok(Produced::Rows(
                scan(input, &inputs)?
                    .into_iter()
                    .map(|(entity, score)| {
                        BTreeMap::from([
                            (columns[0].to_string(), entity),
                            (columns[1].to_string(), Value::Float(score)),
                        ])
                    })
                    .collect(),
            ))
        },
    ))
}

/// The whole-ranking window — no skip, no cap: the `db.index.fulltext.query*`
/// procedures' answer when their `options` map omits `skip` and `limit`.
const RANKING_ALL: (usize, usize) = (0, usize::MAX);

/// The `options` keys the fulltext query procedures read — the one
/// vocabulary the dispatch and the rejection text's list share.
#[derive(Clone, Copy)]
enum OptionKey {
    Skip,
    Limit,
    /// The query-side analyzer: its value is validated by
    /// [`fulltext::Analyzer::from_value`], the same reader `SEARCH`'s
    /// `WITH ANALYZER` uses.
    Analyzer,
}

impl OptionKey {
    const ALL: [Self; 3] = [Self::Skip, Self::Limit, Self::Analyzer];

    fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|option| option.name() == key)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Limit => "limit",
            Self::Analyzer => "analyzer",
        }
    }

    /// The admitted names, for the rejection text.
    fn names() -> String {
        let names: Vec<&str> = Self::ALL.iter().map(|option| option.name()).collect();
        names.join(", ")
    }

    /// Read this key's value: a ranking key assigns its window slot, the
    /// analyzer key its query-side override.
    fn read(
        self,
        value: &Value,
        window: &mut (usize, usize),
        analyzer: &mut Option<fulltext::Analyzer>,
        procedure: &str,
    ) -> Result<()> {
        let slot = match self {
            Self::Skip => &mut window.0,
            Self::Limit => &mut window.1,
            Self::Analyzer => {
                *analyzer = fulltext::Analyzer::from_value(value)?;
                return Ok(());
            }
        };
        let invalid = || {
            Error::argument_error(format!(
                "{procedure} option `{}` expects a non-negative INTEGER",
                self.name()
            ))
        };
        let Value::Int(count) = value else {
            return Err(invalid());
        };
        *slot = usize::try_from(*count).map_err(|_| invalid())?;
        Ok(())
    }
}

/// One `db.index.fulltext.query*` call's `options` map, read into the scan's
/// whole ranking window and query-side analyzer: `skip` and `limit` are
/// non-negative integers, absent meaning no skip and no cap; `analyzer` names
/// the query-side analyzer exactly as `SEARCH`'s `WITH ANALYZER` does (NULL is
/// no override). A key outside that vocabulary and a bad value are typed
/// rejections, never options accepted and ignored.
fn query_options(
    value: &Value,
    procedure: &str,
) -> Result<((usize, usize), Option<fulltext::Analyzer>)> {
    let Value::Map(options) = value else {
        return Err(Error::invalid_argument_type(format!(
            "{procedure} argument `options` expects MAP"
        )));
    };
    let mut window = RANKING_ALL;
    let mut analyzer = None;
    for (key, value) in options.iter() {
        let Some(option) = OptionKey::parse(key) else {
            return Err(Error::argument_error(format!(
                "{procedure} does not accept the `{key}` option (valid: {})",
                OptionKey::names()
            )));
        };
        option.read(value, &mut window, &mut analyzer, procedure)?;
    }
    Ok((window, analyzer))
}

/// The one fulltext scan for both `db.index.fulltext.query*` procedures:
/// `kind` selects the entity kind (relationship indexes match on
/// relationship types, node indexes on labels) and the yielded column
/// name, over the shared scan-call source.
pub(crate) fn fulltext_call_source<'a>(
    ctx: ExecCtx<'a>,
    kind: EntityKind,
    call: CallSpec<'a>,
    signature: &'static ProcSignature,
) -> Result<ComposeSource<'a>> {
    // One corpus per index for the whole call, not one per input row.
    let corpora = fulltext::CorpusMemo::new(ctx.graph);
    scan_call_source(ctx, kind, call, signature, move |input, s| {
        // The fulltext arm's operand read (the twin of the vector arm's
        // `vector_operand`): NULL matches nothing — the
        // null-propagates-to-no-candidates policy every other
        // fulltext/vector operand read answers — a STRING is the query,
        // every other type the typed rejection.
        let query_value = eval(s.ctx.graph, input, &s.args[1], s.ctx.params)?;
        let Some(query) = fulltext::operand(&query_value)? else {
            return Ok(Vec::new());
        };
        // The optional `options` map: `skip`/`limit` page the ranking (the
        // whole ranking without them) and `analyzer` overrides the query side
        // exactly as `SEARCH`'s `WITH ANALYZER` does.
        let (window, query_analyzer) = match s.args.get(2) {
            Some(expr) => query_options(
                &eval(s.ctx.graph, input, expr, s.ctx.params)?,
                signature.name,
            )?,
            None => (RANKING_ALL, None),
        };
        // The shared catalog-descriptor phase (the SEARCH orchestration's
        // one spelling, `fulltext::resolve_analyzers`): index resolution,
        // stored analyzer options, the two-sided analyzers with the
        // query-side override. Only the entity-kind disagreement is this
        // procedure's own, classified before scanning.
        let scan = fulltext::resolve_analyzers(s.ctx.graph, &s.index_name, query_analyzer)?;
        entity_kind_mismatch(s.kind, scan.detail.entity_kind(), "fulltext")?;
        Ok(
            // The procedure scans carry no SEARCH clause, so no
            // inner-WHERE filters ride the ranking (ADR-0678's gate is
            // the clause's); the survivors materialize through the one
            // ranked composition (exec::candidates) with the procedure's
            // own kind, already checked against the index's.
            ranked_candidates(
                s.ctx.graph,
                s.kind,
                fulltext::search(&corpora, s.ctx.graph, scan, query, window.0, window.1, &[])?,
            ),
        )
    })
}

/// The vector scan call's composition source — over the shared scan-call
/// source, like the fulltext scan's ([`fulltext_call_source`]): the same
/// entity-kind classification, the same operand policy, the same typed
/// index-kind comparison.
pub(crate) fn vector_call_source<'a>(
    ctx: ExecCtx<'a>,
    kind: EntityKind,
    call: CallSpec<'a>,
    signature: &'static ProcSignature,
) -> Result<ComposeSource<'a>> {
    scan_call_source(ctx, kind, call, signature, move |input, s| {
        let limit = match eval(s.ctx.graph, input, &s.args[1], s.ctx.params)? {
            Value::Int(value) if (1..=i32::MAX as i64).contains(&value) => value as usize,
            _ => {
                return Err(Error::invalid_argument_value(
                    "numberOfNearestNeighbours must be a positive INTEGER",
                ))
            }
        };
        let query = eval(s.ctx.graph, input, &s.args[2], s.ctx.params)?;
        let Some(query) = vector_operand(&query)? else {
            return Ok(Vec::new());
        };
        // The procedure path carries no SEARCH clause: it resolves the
        // index, classifies it against the procedure's own kind before
        // scanning, and scans with no prepared filters — its fulltext
        // counterpart's exact shape.
        let scan = resolve_vector_index(s.ctx.graph, &s.index_name, &query)?;
        entity_kind_mismatch(s.kind, scan.detail.entity_kind(), "vector")?;
        // `s.kind` equals the resolved index's kind: the mismatch check
        // above rejected the other case.
        let candidates = scan_candidates(s.ctx, &scan, &query, limit, s.kind, &[])?;
        Ok(candidates)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_options_map_reads_skip_limit_and_the_query_analyzer() {
        use crate::test_support::value_map;
        let procedure = "db.index.fulltext.queryNodes";
        let read = |options: &[(&str, Value)]| query_options(&value_map(options), procedure);
        assert_eq!(read(&[]).unwrap(), (RANKING_ALL, None));
        assert_eq!(read(&[("limit", Value::Int(2))]).unwrap(), ((0, 2), None));
        assert_eq!(
            read(&[("skip", Value::Int(1)), ("limit", Value::Int(2))]).unwrap(),
            ((1, 2), None)
        );
        assert_eq!(
            read(&[("skip", Value::Int(2))]).unwrap(),
            ((2, usize::MAX), None)
        );
        // `analyzer` runs the same validation as SEARCH's WITH ANALYZER; a
        // NULL value is no override.
        assert_eq!(
            read(&[("analyzer", Value::String("ENGLISH".into()))]).unwrap(),
            (RANKING_ALL, Some(fulltext::Analyzer::English))
        );
        assert_eq!(
            read(&[("analyzer", Value::Null)]).unwrap(),
            (RANKING_ALL, None)
        );
        // Every admitted key is one of the table's rows: the ranking keys
        // read an integer, the analyzer its own value form.
        for option in OptionKey::ALL {
            let value = match option {
                OptionKey::Skip | OptionKey::Limit => Value::Int(1),
                OptionKey::Analyzer => Value::Null,
            };
            assert!(read(&[(option.name(), value)]).is_ok(), "{}", option.name());
        }
        // A non-map argument, an unknown key and a bad value are rejections,
        // never options accepted and ignored.
        for bad in [
            Value::Int(1),
            value_map(&[("limit", Value::Int(-1))]),
            value_map(&[("limit", Value::String("two".into()))]),
            value_map(&[("count", Value::Int(1))]),
        ] {
            let error = query_options(&bad, procedure).expect_err("rejected");
            assert!(error.to_string().contains("option"), "{bad:?}: {error}");
        }
        // An unknown analyzer name is the analyzer reader's own rejection.
        let error = query_options(
            &value_map(&[("analyzer", Value::String("not-a-real-analyzer".into()))]),
            procedure,
        )
        .expect_err("rejected");
        assert!(error.to_string().contains("analyzer name"), "{error}");
    }
}
