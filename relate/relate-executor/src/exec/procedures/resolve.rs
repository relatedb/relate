//! The generic registry path a non-diverted call takes: the one resolution
//! of a registered or built-in name ([`resolve_registry`]) — a void call's
//! per-row binding, or the composition source a row-producing call streams
//! or materializes through (`compose.rs`) — and the per-row argument
//! binding it reads. Named for the resolution, not the registry: the
//! signature registry itself is `crate::procedure_registry`.

use std::collections::BTreeMap;
use std::rc::Rc;

use relate_core::{Error, Result, Value};
use relate_ir::CallArgs;

use crate::ctx::ExecCtx;
use crate::graph::Procedure;
use crate::procedure_registry::ProcSignature;
use crate::row::Row;

use super::super::compose::{ComposeSource, Produced};
use super::super::procedure_rows::{
    builtin_procedure_rows, executable_signature, procedure_metadata,
};
use super::contract::{call_arg_value, call_args_match, CallSpec};

/// Which rows one resolved call answers: the stored rows shared by every
/// input row (a registered procedure's, or a no-argument built-in's
/// once-produced set — matched by index, cloned never), or a producing
/// signature whose rows are a function of each row's bound arguments.
enum CallRows {
    Shared(Rc<Vec<BTreeMap<String, Value>>>),
    PerRow(&'static ProcSignature),
}

/// One resolved CALL's binding context: the facts every per-row bind needs,
/// carried once instead of as nine positional parameters. The metadata is
/// owned (a parked call's source carries the binding past the dispatcher's
/// rendered-name local) and carries the call's own name — a registered
/// procedure is looked up by it and `procedure_metadata` sets it from the
/// signature, so it is never stored twice.
pub(crate) struct CallBinding<'a> {
    source: &'a CallArgs,
    metadata: Procedure,
    required_args: usize,
    ctx: ExecCtx<'a>,
}

impl<'a> CallBinding<'a> {
    /// The one construction, checking the metadata's one construction
    /// invariant — a declared type per declared argument
    /// (`procedure_metadata` and registration build the two lists
    /// together, so a shorter type list is an engine bug, not a
    /// per-argument absence) — once per call, never per row.
    fn new(
        source: &'a CallArgs,
        mut metadata: Procedure,
        required_args: usize,
        ctx: ExecCtx<'a>,
    ) -> Result<Self> {
        if metadata.arg_types.len() != metadata.arg_names.len() {
            return Err(Error::internal(format!(
                "procedure '{}' declares {} arguments but {} types",
                metadata.name,
                metadata.arg_names.len(),
                metadata.arg_types.len()
            )));
        }
        // The declared types normalize once here, into the vocabulary's
        // own form — the per-row check below then reads
        // `matches_normalized_type` over the precomputed form and never
        // re-trims, re-strips or re-uppercases the same strings (the same
        // normalization the facade's `value_matches_proc_type` applies,
        // so a lowercase or nullable declared spelling behaves alike on
        // both sides).
        for ty in &mut metadata.arg_types {
            *ty = crate::procedure_registry::normalize_proc_type(ty);
        }
        Ok(Self {
            source,
            metadata,
            required_args,
            ctx,
        })
    }

    /// Bind one driving row's arguments and type-check them against the
    /// resolved metadata — the one binding policy the row loop and the
    /// resolved call share, so a void procedure's arguments are validated
    /// exactly like a row-answering one's. The genuinely per-row work
    /// alone; the construction invariant is checked once, at construction.
    pub(crate) fn bind_row(&self, row: &Row) -> Result<BTreeMap<String, Value>> {
        if let CallArgs::Implicit = self.source {
            // Only parameters without a default are mandatory; defaulted
            // built-in parameters (e.g. `db.awaitIndexes`'s
            // `timeOutSeconds`) may be omitted.
            let missing_required = self
                .metadata
                .arg_names
                .iter()
                .take(self.required_args)
                .any(|n| !self.ctx.params.contains_key(n) && !row.contains_key(n));
            if missing_required {
                return Err(Error::syntax(format!(
                    "MissingParameter: implicit args for {}",
                    self.metadata.name
                )));
            }
        }
        let mut call_args = BTreeMap::new();
        for (index, (n, ty)) in self
            .metadata
            .arg_names
            .iter()
            .zip(self.metadata.arg_types.iter())
            .enumerate()
        {
            let Some(value) = call_arg_value(&self.ctx, row, self.source, index, n)? else {
                // A defaulted implicit argument may legitimately be absent.
                continue;
            };
            if !crate::procedure_registry::matches_normalized_type(&value, ty) {
                return Err(Error::syntax(format!(
                    "InvalidArgumentType: {} argument `{n}` expects {ty}",
                    self.metadata.name
                )));
            }
            call_args.insert(n.clone(), value);
        }
        Ok(call_args)
    }
}

/// The registry path's one resolution: the contract-checked shape a
/// non-diverted call runs — a void call (its arguments are still bound and
/// type-checked per row, it just produces no rows) or the composition
/// source a row-producing call streams or materializes through. One
/// resolution, read by both the parked and the materializing form.
pub(crate) fn resolve_registry<'a>(
    ctx: ExecCtx<'a>,
    call: CallSpec<'a>,
) -> Result<super::CallResolution<'a>> {
    let ExecCtx {
        graph, principal, ..
    } = ctx;
    let CallSpec {
        name,
        source,
        yield_items,
        optional,
        where_clause,
    } = call;
    // The registered lookup and the void fact render the name's text once
    // each, through the clause's own bridge — no per-clause allocation.
    let registered = name.with_str(|name| graph.procedure(name));
    // The void fact from its canonical home (`is_void_procedure`, the same
    // helper the facade's compile-time CALL validator reads): a registered
    // procedure that declares no yields is void here too, stored rows or
    // not — the dispatcher's classification cannot drift from the
    // validator's.
    let void = name
        .with_str(|name| crate::procedure_registry::is_void_procedure(registered.as_ref(), name));
    // One resolution decides registered-vs-builtin and carries everything
    // the call needs: the binding (metadata, arity) — both paths need it —
    // then, on the live path alone, the row source. A void call produces
    // no rows at all, so no row source is built and nothing is produced to
    // be discarded.
    let (mut binding, builtin) = match registered {
        Some(procedure) => {
            let required_args = procedure.arg_names.len();
            (
                CallBinding::new(source, procedure, required_args, ctx)?,
                None,
            )
        }
        None => {
            let signature = name.with_str(|name| {
                executable_signature(name).ok_or_else(|| Error::procedure_not_found(name))
            })?;
            (
                CallBinding::new(
                    source,
                    procedure_metadata(signature),
                    signature.required_args(),
                    ctx,
                )?,
                Some(signature),
            )
        }
    };
    // Same rule on the unit-call path (LABS-389): required_args already
    // folds the builtin/registered split, so the guard reads it instead
    // of any-args. The one contract check every dispatch path reads
    // (CallSpec::check_contract), over this resolution's own pair: the
    // passing-mode backstop plus the explicit list's arity — the
    // paren-less form carries no positional arity here, its arguments
    // resolving by name in the binder below (the contract alone is this
    // path's to check; the positional slice is the scans').
    call.check_contract(binding.required_args, binding.metadata.arg_names.len())?;
    // ADR-0699 backstop, ahead of the void classification: a compat
    // callable's CALL was supposed to be rewritten by the front-end; one
    // reaching this resolution means the injection was bypassed, and a
    // VOID entry would otherwise answer the arm below as a silent
    // success. Loud, never a no-op.
    if let Some(signature) = builtin {
        if relate_front::compat_callables::is_compat_callable(signature.name) {
            return Err(super::super::procedure_rows::compat_rewrite_bypassed(
                signature.name,
            ));
        }
    }
    if void {
        return Ok(super::CallResolution::Void { binding });
    }
    // The row source, on the live path alone: a registered procedure's
    // stored rows (moved out of the binding's metadata — the registry's
    // own clone is the only one, never a second), or a no-argument
    // built-in's once-produced set, or a producing signature whose rows
    // are a function of each row's bound arguments.
    let row_source = match builtin {
        None => CallRows::Shared(Rc::new(std::mem::take(&mut binding.metadata.rows))),
        Some(signature) if signature.args.is_empty() => CallRows::Shared(Rc::new(
            builtin_procedure_rows(graph, signature, principal, &BTreeMap::new())?,
        )),
        Some(signature) => CallRows::PerRow(signature),
    };
    let default_yields: Vec<String> = binding.metadata.yield_names.clone();
    let producer = RegistryProducer {
        binding,
        rows: row_source,
    };
    let defaults: Vec<&str> = default_yields.iter().map(String::as_str).collect();
    Ok(super::CallResolution::Compose(ComposeSource::new(
        &defaults,
        yield_items,
        optional,
        where_clause,
        move |row| producer.produce(row),
    )))
}

/// The registry path's per-row producer: the resolved binding and row
/// source, and the argument-matching predicate it reads over every
/// produced row — the production half of [`resolve_registry`], named so
/// the resolution reads as the resolution.
struct RegistryProducer<'a> {
    binding: CallBinding<'a>,
    rows: CallRows,
}

impl RegistryProducer<'_> {
    /// The argument-matching predicate over one produced row — the one
    /// spelling, shared by the stored sets and the per-row productions.
    fn arg_matched(
        &self,
        call_args: &BTreeMap<String, Value>,
        pr: &BTreeMap<String, Value>,
    ) -> bool {
        self.binding
            .metadata
            .arg_names
            .iter()
            .all(|n| match (pr.get(n), call_args.get(n)) {
                (Some(a), Some(b)) => call_args_match(a, b),
                (None, _) => true,
                _ => false,
            })
    }

    /// One input row's production: a stored set its arguments match by
    /// index (never cloned — the composition reads the yielded cells
    /// alone), or the row's own bound-argument production (a null
    /// `db.schema.visualization` options map selects nothing at all; a
    /// `db.stats.retrieve` section names the section it serves) — answered
    /// as the row's own owned rows, not re-filtered: the match predicate
    /// is the stored set's, and a producer echoing a derived value under
    /// an argument's name is its answer, not a row to drop.
    fn produce(&self, row: &Row) -> Result<Produced> {
        let call_args = self.binding.bind_row(row)?;
        Ok(match &self.rows {
            CallRows::Shared(rows) => {
                let matched: Vec<usize> = rows
                    .iter()
                    .enumerate()
                    .filter(|(_, pr)| self.arg_matched(&call_args, pr))
                    .map(|(i, _)| i)
                    .collect();
                Produced::Shared {
                    rows: Rc::clone(rows),
                    matched,
                }
            }
            CallRows::PerRow(signature) => Produced::Rows(builtin_procedure_rows(
                self.binding.ctx.graph,
                signature,
                self.binding.ctx.principal,
                &call_args,
            )?),
        })
    }
}
