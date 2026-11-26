//! The `db.ontology.*` procedures (ADR-0121 as amended by ADR-0227, the
//! shells revised per ADR-0667): `set`, `get`, `drop`, `apply`. No database argument — each acts on the
//! session/`USE`-target database, which the facade's plan-time gates
//! restrict to `neo4j` before execution.
//!
//! Chosen seams (the executor's `ExecCtx`/`ExecRequest` carry no store
//! handle):
//!
//! - `set`/`drop` push the store-level [`RedoOp::OntologySet`] /
//!   [`RedoOp::OntologyDrop`] into the statement's write overlay, exactly
//!   like a catalog op. `set`'s argument runs through the crate codec
//!   first (ADR-0667): a `null` or empty argument pushes the clear — the
//!   same redo `drop` pushes — and a non-empty one is canonicalized before
//!   the redo, so validation failures fail the statement before any WAL
//!   write (the WAL preflight's 16 MiB envelope check backstops it).
//! - `get` is an ordinary read row producer; the view's ontology document
//!   ([`ExecGraph::with_ontology_document`], overridden by the overlay's
//!   pending op through [`ExecGraph::effective_ontology_document`]) is
//!   re-rendered in the requested format through the same codec
//!   (ADR-0667), so `YIELD`/`RETURN` compose as for any procedure.
//! - `apply` is a row-producing in-overlay write procedure: it converts
//!   the stored document with [`relate_ontology::to_graph_type_body`],
//!   runs ADR-0226 §3's validate-first scan against the captured
//!   snapshot, and only then pushes the `ALTER CURRENT GRAPH TYPE SET`
//!   redo through the existing pipeline (`apply_verb` →
//!   `derive_constraints_typed` → `sync_derived_constraints` →
//!   `RedoOp::SetCurrentGraphType`), answering one delta row
//!   (ADR-0667): the applied model's name else id, the constraints the
//!   statement created and dropped, and the converter's skip reports.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Error, Result, Value};
use relate_ontology::{
    parse_stored_ontology_document, render_ontology_document, DocumentFormat, OntologyError,
};

use crate::ctx::ExecCtx;
use crate::overlay::ExecGraph;
use crate::redo::RedoOp;
use crate::row::Row;

use super::super::compose::{ComposeSource, Produced};
use super::CallSpec;
use relate_ir::{CallArgs, Expr, YieldItems};

/// The database every stored ontology belongs to (ADR-0121 §2) — the
/// `database` payload the store-level redo ops carry.
const ONTOLOGY_DATABASE: &str = "neo4j";

/// The four `db.ontology.*` procedures (ADR-0121/ADR-0227, revised by
/// ADR-0667), classified ONCE at the `exec_call` divert by the single
/// `OntologyProc::try_from` classification below — the divert site's one
/// name-matching scheme — so every downstream dispatch matches this enum
/// exhaustively instead of re-matching name strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OntologyProc {
    Get,
    Set,
    Drop,
    Apply,
}

/// The ontology store-op vocabulary — the two void writes
/// [`exec_ontology_write_call`] runs, partitioned from the row-producing
/// `get`/`apply`, so the write path's own type cannot carry a variant it
/// never runs.
#[derive(Debug, Clone, Copy)]
pub(crate) enum OntologyWrite {
    Set,
    Drop,
}

impl OntologyProc {
    /// Every dispatch arm, in this enum's order (`get`, `set`, `drop`,
    /// `apply`) — deliberately not the registry table's `set`, `get`,
    /// `drop`, `apply` order — the one name list `try_from` matches (each
    /// arm's [`Self::name`]), and the dispatch side of the both-ways pin
    /// against the registry's ontology family.
    const ALL: [Self; 4] = [Self::Get, Self::Set, Self::Drop, Self::Apply];

    /// The registered procedure name this arm dispatches. This is the
    /// dispatch vocabulary's only spelling of the name; the registry
    /// signature of the same name is the runtime contract the divert
    /// checks, so the two cannot drift (pinned by test).
    fn name(self) -> &'static str {
        match self {
            Self::Get => "db.ontology.get",
            Self::Set => "db.ontology.set",
            Self::Drop => "db.ontology.drop",
            Self::Apply => "db.ontology.apply",
        }
    }
}

impl TryFrom<&str> for OntologyProc {
    type Error = ();

    /// Classify a registered `db.ontology.*` name — the divert site's
    /// single classification step. The [`Self::ALL`] scan is the only
    /// membership decision: a name matching one arm's [`Self::name`] is
    /// that arm, and the arm's own name is the dispatch key. `Err(())` for
    /// anything else: the call falls to the generic registry path, where an
    /// unregistered name fails with `ProcedureNotFound` and a
    /// user-registered one runs. The both-ways set test pins `ALL` against
    /// the registry's ontology family, so a registry row added or dropped
    /// without the matching arm fails the suite.
    fn try_from(name: &str) -> std::result::Result<Self, ()> {
        Self::ALL
            .into_iter()
            .find(|proc| proc.name() == name)
            .ok_or(())
    }
}

/// Execute one `db.ontology.*` store-op call over the input rows: the void
/// write op runs once per input row and passes the rows through
/// unchanged; the void-optional pad's own row is answered as data — the
/// clause arm completes it. The store-op vocabulary is narrowed here (the
/// row-producing arms answer their composition sources through the
/// resolution). The resolution site (`resolve_call`) classified the name
/// once and ran the runtime contract over the classification's own
/// signature; nothing is re-checked here.
pub(crate) fn exec_ontology_write_call(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    call: CallSpec<'_>,
    write: OntologyWrite,
) -> Result<(Vec<Row>, Option<Row>)> {
    let CallSpec {
        name,
        source,
        optional,
        ..
    } = call;
    // The void-procedure policy's one spelling (the shared helper): the
    // void write op runs once per input row, the pad is the unit row's
    // own.
    let pad = super::pad_or_each(&rows, optional, |row| {
        exec_ontology_write(ctx, row, write, name, source)
    })?;
    Ok((rows, pad))
}

/// One void store op for one input row, over the narrowed store-op
/// vocabulary. The name renders to text only on the error branches, never
/// per row.
fn exec_ontology_write(
    ctx: ExecCtx<'_>,
    row: &Row,
    write: OntologyWrite,
    name: &relate_names::SmallName,
    source: &CallArgs,
) -> Result<()> {
    let graph = ctx.graph;
    if !graph.has_write_overlay() {
        // Unreachable by construction: the facade routes write-procedure
        // statements to the write path. Fail closed rather than panic on
        // the read-only view.
        return name.with_str(|name| {
            Err(Error::internal(format!(
                "{name} executed on a read-only graph view"
            )))
        });
    }
    // Normalize first: `drop` is the clear, and `set`'s `null`/empty
    // argument is the same clear (ADR-0667) — the "null clears" rule is
    // a fact of the one pending-document decision, not of a store-op
    // arm. A non-empty argument is canonicalized through the crate
    // codec here, before the redo, so an invalid document fails the
    // statement before any commit (the 16 MiB ADR-0120 §3 envelope is
    // `RedoOp::validate_ids`, checked by the commit preflight and
    // replay paths).
    let pending = match write {
        OntologyWrite::Drop => None,
        OntologyWrite::Set => set_argument(ctx, row, source, name)?
            .map(|document| canonicalize_argument(&document))
            .transpose()?,
    };
    // One construction of each redo: the set with a document, the clear
    // every cleared path pushes.
    match pending {
        Some(spec) => graph.push_catalog_redo(RedoOp::OntologySet {
            database: ONTOLOGY_DATABASE.to_string(),
            spec,
        }),
        None => graph.push_catalog_redo(RedoOp::OntologyDrop {
            database: ONTOLOGY_DATABASE.to_string(),
        }),
    }
    Ok(())
}

/// The canonical storage form of `set`'s argument (ADR-0667): the crate
/// codec's canonical JSON, each failure rendered as its user-facing
/// envelope in the argument-error class.
fn canonicalize_argument(document: &str) -> Result<String> {
    relate_ontology::canonicalize_ontology_document(document.as_bytes())
        .map_err(|err| Error::argument_error(err.to_string()))
}

/// `set`'s one argument (ADR-0667): a `null` or empty STRING is the clear
/// (`None`), a non-empty STRING the document. The argument is required —
/// absence is the required-argument rejection (the runtime arity contract
/// already refused the explicit empty call, so absence here is the
/// paren-less form), unlike `get`'s defaulted format, which is why this
/// reads through the per-argument resolver instead of the optional reader.
fn set_argument(
    ctx: ExecCtx<'_>,
    row: &Row,
    source: &CallArgs,
    name: &relate_names::SmallName,
) -> Result<Option<String>> {
    match super::contract::call_arg_value(&ctx, row, source, 0, "ontology")? {
        None => name.with_str(|name| {
            Err(Error::invalid_number_of_arguments(format!(
                "{name} argument `ontology` is required"
            )))
        }),
        // `null` and the empty document are the clear.
        Some(Value::Null) => Ok(None),
        Some(Value::String(document)) if document.is_empty() => Ok(None),
        Some(Value::String(document)) => Ok(Some(document.to_string())),
        Some(_) => name.with_str(|name| {
            Err(Error::invalid_argument_type(format!(
                "{name} argument `ontology` expects STRING"
            )))
        }),
    }
}

/// `db.ontology.apply`'s one result row (ADR-0667): the applied model's
/// name else id, the delta the statement applied, and the converter's
/// skip reports.
struct ApplyOutcome {
    ontology: Arc<str>,
    created: usize,
    dropped: usize,
    skipped: Vec<String>,
}

/// `db.ontology.apply` (ADR-0226 §2/§3, revised by ADR-0667): read the
/// effective document — the pending one when the calling transaction set
/// or dropped it — parse and validate it through the stored-document
/// codec, convert it to the graph-type body, and run the shared `ALTER
/// CURRENT GRAPH TYPE SET` pipeline with the validate-first scan
/// (ADR-0226 §3) spliced in before the first catalog redo. Answers the
/// delta row's four cells.
fn exec_ontology_apply(graph: &ExecGraph<'_>) -> Result<ApplyOutcome> {
    let Some(document) = graph.effective_ontology_document() else {
        return Err(Error::procedure_call(
            "This database has no stored ontology.",
        ));
    };
    // ADR-0364 §4: `apply` is the one operation that must interpret the
    // stored document into the typed model. The stored bytes were
    // accepted by `set` at the time they were committed, but a document
    // written by an older binary never passed today's gate — a parse or
    // version failure is the undecodable-stored-document rejection, a
    // validation failure the stored-document envelope, both loud
    // procedure-call failures (ADR-0667), never internal ones.
    let doc = parse_stored_ontology_document(document.as_bytes()).map_err(stored_document_error)?;
    let mapping = relate_ontology::to_graph_type_body(&doc)
        .map_err(|err| Error::procedure_call(err.to_string()))?;
    let delta = super::super::graph_type_schema::set_current_graph_type_apply(
        graph,
        relate_parser::AlterGraphTypeVerb::Set,
        &mapping.body,
    )?;
    Ok(ApplyOutcome {
        ontology: doc.name().unwrap_or_else(|| doc.id()).into(),
        created: delta.created,
        dropped: delta.dropped,
        skipped: mapping.skipped,
    })
}

/// The `db.ontology` family's one rendering of a stored-document codec
/// failure (ADR-0667): a document-shaped failure — the stored bytes did
/// not decode as a document at all — is wrapped as an undecodable stored
/// document; the validation envelope is already the stored-document
/// wording, verbatim. `get`'s re-render and `apply`'s interpretation
/// route their decode failures through this one path, so the class and
/// the framing cannot drift between the family's two readers; the
/// document-shaped classification itself is the ontology crate's own
/// ([`OntologyError::is_document_shaped`], exhaustive over the
/// variants).
fn stored_document_error(err: OntologyError) -> Error {
    if err.is_document_shaped() {
        Error::procedure_call(format!(
            "The stored ontology is not a valid ontology document: {err}"
        ))
    } else {
        Error::procedure_call(err.to_string())
    }
}

/// `db.ontology.get(format = "yaml") :: (ontology :: STRING)` (ADR-0227
/// §3, revised by ADR-0667): the effective document — the pending one when
/// the calling transaction set or dropped it — re-rendered in the
/// requested format through the same codec, `null` when no ontology is
/// set. The document resolves once per clause execution — the overlay
/// cannot change during a read clause's row production — and the
/// one-per-row producer only re-renders it in each row's requested
/// format. Answers its composition source — the one-per-row producer
/// under the shared compose policy (YIELD defaulting with the typed
/// star/default modes, aliasing, WHERE, OPTIONAL padding) — so the
/// resolution carries it like every other row-producing call: a folding
/// consumer parks it (ADR-0555), a plain projection materializes it with
/// the ADR-0463 charge. The signature's declared return columns are the
/// default yields and the produced cells' keys — the registry row is the
/// one column table, exactly as the scan sources read it.
pub(crate) fn ontology_get_source<'a>(
    ctx: ExecCtx<'a>,
    call: CallSpec<'a>,
    signature: &'static crate::procedure_registry::ProcSignature,
) -> Result<ComposeSource<'a>> {
    let CallSpec {
        source,
        name,
        yield_items,
        optional,
        where_clause,
    } = call;
    // The effective document resolves once per clause execution, never
    // per produced row: the source constructs at this clause's own
    // execution point — after the clauses before it ran — and nothing
    // can push an ontology op between this clause's rows (a pending op
    // comes from a write clause's redo, never from a read's row
    // production), so every row of one clause answers the same
    // document without copying the pending one per row.
    let document = ctx.graph.effective_ontology_document();
    // The one declared return column, bound once at construction: the
    // registry signature's returns table is the row's one column — a
    // static fact of the registry row, never a per-row runtime index.
    Ok(compose_ontology_source(
        signature,
        yield_items,
        optional,
        where_clause,
        move |columns| {
            let column = *columns
                .first()
                .expect("the get signature declares its one return column");
            move |input: &Row| {
                let format = format_arg(ctx, input, source, name)?;
                let value = match &document {
                    None => Value::Null,
                    // A stored document that no longer decodes fails through
                    // the family's one stored-document error path — the same
                    // class and framing `apply` fails with (ADR-0667), never
                    // an argument error blaming the format argument.
                    Some(document) => Value::String(
                        render_ontology_document(document.as_bytes(), format)
                            .map_err(stored_document_error)?
                            .into(),
                    ),
                };
                let mut row = BTreeMap::new();
                row.insert(column.to_string(), value);
                Ok(Produced::Rows(vec![row]))
            }
        },
    ))
}

/// `db.ontology.apply() :: (ontology :: STRING, constraintsCreated ::
/// INTEGER, constraintsDropped :: INTEGER, skipped :: LIST<STRING>)`
/// (ADR-0667): the row-producing write arm — the pipeline runs once per
/// input row (a standalone call is one input row, so one row) and pushes
/// its catalog redos into the statement's overlay exactly like the DDL it
/// mirrors, then answers the delta row through the same compose policy as
/// `get`, its columns read from the registry signature like every
/// row-producing call's.
pub(crate) fn ontology_apply_source<'a>(
    ctx: ExecCtx<'a>,
    call: CallSpec<'a>,
    signature: &'static crate::procedure_registry::ProcSignature,
) -> Result<ComposeSource<'a>> {
    let CallSpec {
        yield_items,
        optional,
        where_clause,
        ..
    } = call;
    Ok(compose_ontology_source(
        signature,
        yield_items,
        optional,
        where_clause,
        move |columns| {
            move |_input: &Row| {
                let ApplyOutcome {
                    ontology,
                    created,
                    dropped,
                    skipped,
                } = exec_ontology_apply(ctx.graph)?;
                let cells = [
                    Value::String(ontology),
                    Value::Int(created as i64),
                    Value::Int(dropped as i64),
                    Value::List(
                        skipped
                            .iter()
                            .map(|report| Value::String(report.as_str().into()))
                            .collect::<Vec<_>>()
                            .into(),
                    ),
                ];
                let row: BTreeMap<String, Value> = columns
                    .iter()
                    .zip(cells)
                    .map(|(column, cell)| (column.to_string(), cell))
                    .collect();
                Ok(Produced::Rows(vec![row]))
            }
        },
    ))
}

/// The source prelude both `db.ontology.*` row producers share (ADR-0667):
/// the registry signature's declared return columns are the one column
/// table — the defaults handed to the composition's YIELD resolution and
/// the keys the produced cells land under. `bind` receives that table once
/// per clause execution (a static fact of the registry row, never per-row
/// work) and answers the one-per-row producer.
fn compose_ontology_source<'a, P>(
    signature: &'static crate::procedure_registry::ProcSignature,
    yield_items: &YieldItems,
    optional: bool,
    where_clause: Option<&'a Expr>,
    bind: impl FnOnce(Vec<&'static str>) -> P,
) -> ComposeSource<'a>
where
    P: FnMut(&Row) -> Result<Produced> + 'a,
{
    let columns: Vec<&'static str> = signature.returns.iter().map(|ret| ret.name).collect();
    ComposeSource::new(
        &columns,
        yield_items,
        optional,
        where_clause,
        bind(columns.clone()),
    )
}

/// `get`'s format argument decode (ADR-0667): absent or `null` is `yaml`,
/// the two names are case-insensitive, and any other value is the
/// argument-error rejection naming the vocabulary — the one list
/// [`DocumentFormat::ALL`] that `from_name` searches, so the message
/// cannot drift from the decode's acceptance set.
fn format_arg(
    ctx: ExecCtx<'_>,
    row: &Row,
    source: &CallArgs,
    name: &relate_names::SmallName,
) -> Result<DocumentFormat> {
    match super::contract::optional_string_arg(ctx, row, source, 0, "format", name)? {
        None => Ok(DocumentFormat::Yaml),
        Some(format) => {
            let lowered = format.to_ascii_lowercase();
            DocumentFormat::from_name(&lowered).ok_or_else(|| {
                let valid = DocumentFormat::ALL
                    .iter()
                    .map(|known| format!("'{}'", known.as_str()))
                    .collect::<Vec<_>>()
                    .join(" and ");
                Error::argument_error(format!(
                    "Unknown ontology format '{format}'. Valid formats are {valid}."
                ))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_names::SmallName;

    /// The dispatch vocabulary, spelled literally once in test: each arm is
    /// reachable from exactly its registered procedure name. (`Self::name`
    /// is the implementation's key; this independent pin catches an arm and
    /// a name drifting apart.)
    #[test]
    fn ontology_proc_classifies_its_registered_names() {
        let dispatch_names = [
            (OntologyProc::Get, "db.ontology.get"),
            (OntologyProc::Set, "db.ontology.set"),
            (OntologyProc::Drop, "db.ontology.drop"),
            (OntologyProc::Apply, "db.ontology.apply"),
        ];
        for (proc, name) in dispatch_names {
            assert_eq!(proc.name(), name);
            assert_eq!(OntologyProc::try_from(name), Ok(proc), "{name}");
            let signature = crate::procedure_registry::builtin_procedure_signature(name)
                .unwrap_or_else(|| panic!("{name} must be registered"));
            // The enum's read/write partition and the registry's flags are
            // one fact — pinned here so neither home can drift from the
            // other. The row-producing `apply` writes without being a
            // store op; the store ops are void; `get` reads.
            assert_eq!(
                matches!(proc, OntologyProc::Set | OntologyProc::Drop),
                signature.store_level,
                "{name}: the store-op vocabulary must match the registry's store-level flag"
            );
            assert_eq!(
                !matches!(proc, OntologyProc::Get),
                signature.writes,
                "{name}: every arm but get writes"
            );
            assert_eq!(
                matches!(proc, OntologyProc::Set | OntologyProc::Drop),
                signature.is_void(),
                "{name}: the store-op vocabulary must be exactly the void signatures"
            );
            assert_eq!(
                proc == OntologyProc::Apply,
                signature.writes && !signature.is_void(),
                "{name}: apply is the one row-producing write"
            );
        }
        assert!(OntologyProc::try_from("db.ontology.nope").is_err());
        assert!(OntologyProc::try_from("db.labels").is_err());
    }

    /// The divert's single classification and the registry's ontology
    /// predicate name the same set, in both directions: every registered
    /// `db.ontology.*` signature has a dispatch arm, and `try_from` accepts
    /// no name the registry does not register (the facade's gates consume
    /// the predicate, the executor's divert consumes the classification —
    /// they cannot drift apart).
    #[test]
    fn classification_and_registry_name_the_same_set_both_ways() {
        use std::collections::BTreeSet;

        let registered: BTreeSet<&str> = crate::procedure_registry::BUILTIN_PROCEDURE_SIGNATURES
            .iter()
            .filter(|signature| crate::procedure_registry::is_ontology_procedure(signature.name))
            .map(|signature| signature.name)
            .collect();
        let classified: BTreeSet<&str> = OntologyProc::ALL.iter().map(|proc| proc.name()).collect();
        assert_eq!(classified, registered);

        for signature in crate::procedure_registry::BUILTIN_PROCEDURE_SIGNATURES {
            assert_eq!(
                OntologyProc::try_from(signature.name).is_ok(),
                registered.contains(signature.name),
                "{}",
                signature.name
            );
        }
    }

    /// `get`'s format decode (ADR-0667): absent and null default to yaml,
    /// the names decode case-insensitively, and an unknown value is the
    /// exact argument-error text naming the two.
    #[test]
    fn format_arg_decodes_case_insensitively_and_rejects_unknown_values() {
        let graph = crate::graph::Graph::default();
        let view = ExecGraph::read(&graph);
        let params = std::collections::BTreeMap::new();
        let ctx = ExecCtx::new(&view, &params);
        let row = Row::new();
        let name = &SmallName::from_name("db.ontology.get");
        for (given, expected) in [
            (None, DocumentFormat::Yaml),
            (Some("yaml"), DocumentFormat::Yaml),
            (Some("YAML"), DocumentFormat::Yaml),
            (Some("YamL"), DocumentFormat::Yaml),
            (Some("json"), DocumentFormat::Json),
            (Some("JSON"), DocumentFormat::Json),
            (Some("Json"), DocumentFormat::Json),
        ] {
            let source = given
                .map(|value| {
                    relate_ir::CallArgs::Explicit(vec![relate_ir::Expr::String(value.to_string())])
                })
                .unwrap_or_else(|| relate_ir::CallArgs::Explicit(Vec::new()));
            assert_eq!(
                format_arg(ctx, &row, &source, name).unwrap_or_else(|e| panic!("{given:?}: {e}")),
                expected
            );
        }
        // A null argument is the default too.
        assert_eq!(
            format_arg(
                ctx,
                &row,
                &CallArgs::Explicit(vec![relate_ir::Expr::Null]),
                name
            )
            .expect("a null format argument defaults"),
            DocumentFormat::Yaml
        );
        // An unknown value fails with the exact text.
        let err = format_arg(
            ctx,
            &row,
            &CallArgs::Explicit(vec![relate_ir::Expr::String("xml".to_string())]),
            name,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .ends_with("Unknown ontology format 'xml'. Valid formats are 'json' and 'yaml'."),
            "{err}"
        );
        // A non-string argument is a type error.
        let err = format_arg(
            ctx,
            &row,
            &CallArgs::Explicit(vec![relate_ir::Expr::Int(1)]),
            name,
        )
        .unwrap_err();
        assert!(err.to_string().contains("InvalidArgumentType"), "{err}");
    }

    /// The generic path's passing-mode backstop covers the ontology divert
    /// too: an implicit in-query `db.ontology.get` — the one ontology
    /// procedure with an argument — with a YIELD fails exactly like the
    /// generic path's guard does for a registered procedure with an
    /// argument. (The facade's compile-time `validate_calls` raises the
    /// same error earlier on every facade-routed statement; this drives
    /// `exec_call` directly to pin the executor-side backstop.)
    #[test]
    fn implicit_get_with_defaulted_arg_succeeds_but_required_arg_is_a_passing_mode_error() {
        let graph = crate::graph::Graph::default();
        let params = std::collections::BTreeMap::new();
        let view = ExecGraph::read(&graph);
        let ctx = ExecCtx::new(&view, &params);
        // Through the divert: `exec_call` classifies once, the ontology
        // executor's registry-signature contract raises the error.
        let call = CallSpec {
            name: &SmallName::from_name("db.ontology.get"),
            source: &CallArgs::Implicit,
            yield_items: &relate_ir::YieldItems::Named(vec![(
                SmallName::from_name("ontology"),
                None,
            )]),
            optional: false,
            where_clause: None,
        };
        // LABS-389 (the paren-less follow-up): `db.ontology.get`'s one
        // parameter carries a default (format = yaml), so the implicit
        // in-query call with a YIELD is allowed -- required args key the
        // guard, not declared ones. The call succeeds with the default.
        let ran = match super::super::exec_call_clause(
            ctx,
            vec![Row::new()],
            call,
            super::super::CallPipeline {
                profiled: false,
                parks: false,
                terminal: false,
            },
            Vec::new,
        )
        .expect("the explicit-default call runs")
        {
            super::super::CallClauseOut::Ran { rows, .. } => rows,
            super::super::CallClauseOut::Parked(_) => {
                panic!("an ontology get is never a streamable producer")
            }
        };
        assert!(!ran.is_empty());
        // The mirror on the generic path: a user-registered procedure with
        // an argument, called implicitly in-query with a YIELD, hits the
        // same backstop there.
        graph.register_procedure(crate::graph::Procedure {
            name: "custom.one".to_string(),
            arg_names: vec!["x".to_string()],
            arg_types: vec!["ANY".to_string()],
            yield_names: vec!["out".to_string()],
            rows: Vec::new(),
        });
        let call = CallSpec {
            name: &SmallName::from_name("custom.one"),
            source: &CallArgs::Implicit,
            yield_items: &relate_ir::YieldItems::Named(vec![(SmallName::from_name("out"), None)]),
            optional: false,
            where_clause: None,
        };
        // The mirror (a REQUIRED argument): the implicit in-query call
        // with a YIELD stays rejected -- required args key the guard, and
        // the contract runs at the resolution itself.
        let Err(err) = super::super::resolve_call(ctx, call) else {
            panic!("a required implicit argument with a YIELD must be the passing-mode rejection")
        };
        let text = err.to_string();
        assert!(
            text.contains("InvalidArgumentPassingMode")
                && text.contains("cannot pass arguments implicitly"),
            "{text}"
        );
    }
}
