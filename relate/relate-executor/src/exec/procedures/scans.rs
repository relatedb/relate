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
use crate::exec::candidates::{materialize_candidate, resolve_search_index};
use crate::procedure_registry::ProcSignature;
use crate::row::Row;

use super::super::compose::{ComposeSource, Produced};
use super::super::fulltext;
use super::super::vector::{vector_index_candidates, vector_operand, VectorSearchFilter};
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
    let corpora = fulltext::CorpusMemo::default();
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
        // The shared SEARCH index resolution (the vector procedure's one
        // spelling): a missing or non-fulltext index is that helper's
        // typed error; only the entity-kind disagreement is this
        // procedure's own.
        let (detail, options) = resolve_search_index(
            s.ctx.graph,
            &s.index_name,
            crate::redo::IndexKindSpec::Fulltext,
        )?;
        entity_kind_mismatch(
            s.kind,
            EntityKind::from_relationship_flag(detail.relationship),
            "fulltext",
        )?;
        // The one CIP-225 scan: the Lucene-style query parser (terms
        // OR'd, `AND`/`OR`/`NOT`, `property:term` field filters) and the
        // live-fitted BM25 score, ranked best-first, no window.
        let analyzers = fulltext::Analyzers::new(
            crate::exec::ddl_options::index_analyzer(&options, &s.index_name)?,
            None,
        );
        Ok(
            fulltext::search_all(&corpora, s.ctx.graph, &detail, query, analyzers)?
                .into_iter()
                .map(|(id, score)| (materialize_candidate(s.ctx.graph, s.kind, id), score))
                .collect(),
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
        let (index_kind, candidates) = vector_index_candidates(
            s.ctx,
            &s.index_name,
            &query,
            0,
            limit,
            VectorSearchFilter {
                filters: &[],
                input,
            },
        )?;
        entity_kind_mismatch(s.kind, index_kind, "vector")?;
        Ok(candidates)
    })
}
