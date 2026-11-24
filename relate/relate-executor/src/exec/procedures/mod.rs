//! `CALL` procedure execution: registered + built-in procedures,
//! fulltext and vector index query procedures. The dispatch path resolves
//! a `CALL` once ([`resolve_call`]): a specialized executor (ontology,
//! vector, fulltext) or the generic registry fallback, answered as one
//! [`CallResolution`] both forms read — the parked path streams the
//! composition source a row-producing call carries (ADR-0555), the
//! materializing run executes the resolution's own arms.
//!
//! - [`contract`] — the static clause shape and its one runtime contract,
//!   the argument-value matching predicates, and the void-procedure
//!   unit-row rule
//! - [`ontology`] — the `db.ontology.*` divert's executor
//! - [`resolve`] — the generic registry path's one resolution and the
//!   per-row argument binding
//! - [`scans`] — the diverted vector and fulltext scan sources
//!
//! The built-in procedure row producers live in the sibling
//! `procedure_rows` module; the row composition itself in `compose`; the
//! declared-type vocabulary in `crate::procedure_registry` (beside the
//! type strings it interprets).

mod contract;
mod ontology;
mod resolve;
mod scans;

use relate_core::{Result, Value};
use relate_model::EntityKind;

use crate::ctx::ExecCtx;
use crate::procedure_registry::ProcSignature;
use crate::row::Row;

use super::compose::ComposeSource;
use super::feed::StreamedFeed;
use super::QueryResult;
use ontology::{exec_ontology_write_call, OntologyProc, OntologyWrite};

pub use contract::{check_explicit_arity, check_implicit_passing_mode};
pub(crate) use contract::{pad_or_each, CallSpec};
use resolve::resolve_registry;

/// The one classification step of the `CALL` dispatch path: which
/// specialized executor owns the name — or `None` for the generic registry
/// fallback. The `db.ontology.*` procedures (ADR-0121/ADR-0227) classify
/// by the single [`OntologyProc::try_from`] (their executor's exhaustive
/// dispatch enum); the vector and fulltext query procedures classify by
/// the canonical signature registry — the exact, case-sensitive registered
/// names (CIP-230), the same resolution the generic path's own built-in
/// lookup uses. This replaces the historical ad-hoc divert schemes (a
/// registry scan plus a `starts_with` prefix plus `try_from` for the
/// ontology divert, a lowercase literal match for the vector pair, a
/// lowercase substring match for fulltext): no parallel name-matching
/// scheme remains in the dispatch path.
///
/// Answers beside the variant the canonical registry signature the
/// classification resolved — the one fetch the diverted contracts and the
/// void fact read, never a second lookup.
pub(crate) enum CallDivert {
    /// One of the four `db.ontology.*` procedures, already classified.
    Ontology(OntologyProc),
    /// One of the two `db.index.vector.query*` procedures, with the entity
    /// kind its name selects — the same typed vocabulary the fulltext
    /// variant carries.
    Vector(EntityKind),
    /// One of the two `db.index.fulltext.query*` procedures, with the
    /// entity kind its name selects — the same payload spelling as the
    /// vector variant.
    FulltextQuery(EntityKind),
}

/// The one classification step: decide [`CallDivert`] for one `CALL`
/// name, once, before the generic machinery — with the registry signature
/// that classified it. The whole classifier is keyed on the canonical
/// registered name beside the signature it answers: the ontology family
/// by the signature's own name (their executor's exhaustive dispatch
/// enum), the vector and fulltext families by the exact, case-sensitive
/// registered names (CIP-230) — no parallel name-matching scheme, and no
/// second spelling of the name the classification already resolved.
pub(crate) fn divert_call(
    name: &relate_names::SmallName,
) -> Option<(&'static ProcSignature, CallDivert)> {
    // The canonical execution resolution (the registry row filtered on
    // `executable`) doubles as the classifier: only exact, callable
    // registered names divert, exactly like the generic path's built-in
    // resolution — a divert name flipped to `executable(false)` falls to
    // the generic path's own rejection, it never runs through a divert.
    let signature = name.with_str(crate::exec::procedure_rows::executable_signature)?;
    // The ontology family classifies first, by the signature's own name
    // (their executor's exhaustive dispatch enum, classified once); the
    // vector and fulltext families by the exact registered names.
    let divert = if let Ok(proc) = OntologyProc::try_from(signature.name) {
        CallDivert::Ontology(proc)
    } else {
        match signature.name {
            "db.index.vector.queryNodes" => CallDivert::Vector(EntityKind::Node),
            "db.index.vector.queryRelationships" => CallDivert::Vector(EntityKind::Relationship),
            "db.index.fulltext.queryNodes" => CallDivert::FulltextQuery(EntityKind::Node),
            "db.index.fulltext.queryRelationships" => {
                CallDivert::FulltextQuery(EntityKind::Relationship)
            }
            _ => return None,
        }
    };
    Some((signature, divert))
}

/// One `CALL`'s resolution — the single dispatch decision both forms read:
/// the name is classified once ([`divert_call`]), the contract checked
/// once, the target resolved once. The parked path streams the composition
/// source a row-producing call carries; the materializing run
/// ([`exec_resolved_call`]) executes the resolution's own arms.
pub(crate) enum CallResolution<'a> {
    /// The `db.ontology.set`/`drop` store ops — the narrowed void store-op
    /// vocabulary alone: both declare no return columns, so the variant is
    /// void by construction. The row-producing arms (`get`, `apply`)
    /// answer their composition sources through
    /// [`CallResolution::Compose`] like every row-producing call.
    Ontology { write: OntologyWrite },
    /// A void call: its per-row binding — the arguments are still bound and
    /// type-checked, no rows are produced.
    Void { binding: resolve::CallBinding<'a> },
    /// A row-producing call's composition source — streamed into a folding
    /// consumer (ADR-0555) or materialized with the ADR-0463 charge.
    Compose(ComposeSource<'a>),
}

impl CallResolution<'_> {
    /// Whether the call is a void one — the registry's own fact (CIP-159's
    /// terminal unit result): the ontology signature's `is_void`, the void
    /// resolution's constant, never the compose arm's. Read where the
    /// resolution decides it, never re-derived per arm downstream.
    pub(crate) fn yields_unit(&self) -> bool {
        match self {
            // The ontology store ops are void — enforced by the pin in
            // `ontology.rs` (each store op's registry signature declares
            // no return columns), like the registry path's void
            // resolution.
            CallResolution::Ontology { .. } | CallResolution::Void { .. } => true,
            CallResolution::Compose(_) => false,
        }
    }
}

/// The one resolution of a `CALL`: classify the name once, check the
/// contract over the classification's own signature, and answer the shape
/// both forms read. The `db.ontology.*` divert answers its closed
/// one-per-call document rows through its own executor (never an
/// input-proportional container, so it never streams); a void call through
/// its binding; everything row-producing through the composition source.
pub(crate) fn resolve_call<'a>(ctx: ExecCtx<'a>, call: CallSpec<'a>) -> Result<CallResolution<'a>> {
    Ok(match divert_call(call.name) {
        Some((signature, CallDivert::Ontology(proc))) => {
            // The ontology divert runs the same runtime contract (passing
            // mode, arity) the generic path runs, at this same site, over
            // the classification's own signature — then the one partition
            // into its arms, matched exhaustively over the dispatch enum:
            // `get` and `apply` answer composition sources like every
            // row-producing call (a folding consumer parks them,
            // ADR-0555), the store ops the narrowed void write arm.
            call.check_diverted_contract(signature)?;
            match proc {
                OntologyProc::Get => {
                    CallResolution::Compose(ontology::ontology_get_source(ctx, call, signature)?)
                }
                OntologyProc::Apply => {
                    CallResolution::Compose(ontology::ontology_apply_source(ctx, call, signature)?)
                }
                OntologyProc::Set => CallResolution::Ontology {
                    write: OntologyWrite::Set,
                },
                OntologyProc::Drop => CallResolution::Ontology {
                    write: OntologyWrite::Drop,
                },
            }
        }
        Some((signature, CallDivert::Vector(entity))) => {
            CallResolution::Compose(scans::vector_call_source(ctx, entity, call, signature)?)
        }
        Some((signature, CallDivert::FulltextQuery(kind))) => {
            CallResolution::Compose(scans::fulltext_call_source(ctx, kind, call, signature)?)
        }
        // The generic registry fallback.
        None => resolve_registry(ctx, call)?,
    })
}

/// The dispatcher's `CALL` arm's answer: the clause ran — its rows (the
/// void-optional pad already completed and scope-bound inside), and the
/// terminal void call's statement result — or its generator is parked for
/// the folding consumer that follows (ADR-0555: a `CALL` whose follower
/// folds rows as they arrive streams its composed rows through the feed
/// spine, and they are never held).
pub(crate) enum CallClauseOut<'a> {
    Ran {
        rows: Vec<Row>,
        result: Option<QueryResult>,
    },
    Parked(StreamedFeed<'a>),
}

/// The dispatcher's pipeline facts for one `CALL`, passed in as one named
/// value — this module reads call facts and the dispatcher's own pipeline
/// facts, never the clause pipeline itself.
pub(crate) struct CallPipeline {
    /// A PROFILE sink is attached.
    pub(crate) profiled: bool,
    /// The follower is a projection, which takes the call's generator as a
    /// feed — the same `projection_follows` guard the `LOAD CSV` and
    /// `UNWIND` arms park by.
    pub(crate) parks: bool,
    /// This clause is the part's last.
    pub(crate) terminal: bool,
}

/// The dispatcher's `CALL` arm (stellar round 2, F-2-2): resolve the call
/// once, park its generator for a projection follower — the same
/// `projection_follows` guard `UNWIND` and `LOAD CSV` park by — or run the
/// resolution's own materializing arms, complete the void-optional pad (the
/// scope-name walk `scope_names` answers is paid only when a pad actually
/// fired), and answer the terminal void call's empty unit result.
pub(crate) fn exec_call_clause<'a>(
    ctx: ExecCtx<'a>,
    rows: Vec<Row>,
    call: CallSpec<'a>,
    pipeline: CallPipeline,
    scope_names: impl FnOnce() -> Vec<String>,
) -> Result<CallClauseOut<'a>> {
    // ONE resolution answers both forms: the parked path streams the
    // composition source it carries, the materializing tail below runs the
    // resolution's own arms — never a second classification or resolution.
    // The spec carries the clause's own name (rendered to text only at the
    // read sites); the args, yields and WHERE keep the clause's own
    // borrow.
    let resolution = match resolve_call(ctx, call)? {
        // A projection follower keeps the call's generator: the composed
        // rows are never held (ADR-0555). A void call or the ontology
        // divert — the resolution's other arms — is not a streamable row
        // producer and falls to the materializing tail.
        CallResolution::Compose(source) if pipeline.parks => {
            return Ok(CallClauseOut::Parked(StreamedFeed::new(
                ctx,
                rows,
                Box::new(source),
                pipeline.profiled,
            )));
        }
        resolution => resolution,
    };
    // The terminal-void unit fact, read where the resolution decides it.
    let yields_unit = resolution.yields_unit();
    // The materializing run — the resolution's own three arms: the
    // ontology executor, the void call's per-row binding (which still
    // binds and type-checks each row's arguments, it just produces
    // nothing), or the composition's charged materializing form
    // (ADR-0463).
    let (mut rows, pad) = match resolution {
        CallResolution::Ontology { write, .. } => exec_ontology_write_call(ctx, rows, call, write)?,
        CallResolution::Void { binding } => {
            // The void-procedure policy's one spelling: an OPTIONAL call
            // with no return columns over no input rows pads one unit row
            // — its own row, carried as data.
            let pad = pad_or_each(&rows, call.optional, |row| {
                binding.bind_row(row).map(|_| ())
            })?;
            (rows, pad)
        }
        CallResolution::Compose(source) => (source.compose_into(ctx, rows)?, None),
    };
    // A void optional call over a zero-row driving table yields one unit
    // row (CIP-159 [12]); Neo4j reads every variable in scope as null on
    // it, so bind the scope names explicitly (ADR-0335) onto the pad's
    // own row — the walk paid only here, never per CALL clause.
    if let Some(mut pad) = pad {
        for name in scope_names() {
            pad.insert(name, Value::Null);
        }
        rows.push(pad);
    }
    // A terminal void call yields CIP-159's empty unit result: the fact
    // read off the resolution that decided it.
    let result = (pipeline.terminal && yields_unit).then(QueryResult::unit);
    Ok(CallClauseOut::Ran { rows, result })
}
