//! The `CALL` contract vocabulary: the static clause shape ([`CallSpec`])
//! and its one runtime contract, the argument-value matching predicates,
//! and the void-procedure unit-row rule. (The declared-type vocabulary's
//! canonical home is `crate::procedure_registry`, beside the type strings
//! it interprets; the name classification lives in the module root,
//! beside the resolution that consumes it.)

use relate_core::{Error, Result, Value};
use relate_ir::{CallArgs, Expr, YieldItems};
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::procedure_registry::ProcSignature;
use crate::row::Row;

pub(crate) fn call_args_match(proc_val: &Value, given: &Value) -> bool {
    // `eq_non_null` already covers the Int/Float cross-comparison with the
    // exact rule (`relate_model::canonical_key::eq_i64_f64`); a separate
    // `as f64` fallback here would reintroduce the lossy cast.
    proc_val.eq_non_null(given) || (proc_val.is_null() && given.is_null())
}

/// The one typed required-STRING argument reader the CALL paths share:
/// one declared argument, by explicit position or implicit
/// parameter/row column, a STRING or the typed rejection — the procedure
/// name rendered through the clause's carrier on the error branches
/// alone, so the per-row read path allocates nothing.
pub(crate) fn string_arg(
    ctx: ExecCtx<'_>,
    row: &Row,
    source: &CallArgs,
    index: usize,
    arg_name: &str,
    name: &SmallName,
) -> Result<String> {
    // The required form over the one optional reader: absent/null adds the
    // required-argument rejection.
    match optional_string_arg(ctx, row, source, index, arg_name, name)? {
        Some(value) => Ok(value),
        None => name.with_str(|name| {
            Err(Error::invalid_number_of_arguments(format!(
                "{name} argument `{arg_name}` is required"
            )))
        }),
    }
}

/// The optional counterpart of [`string_arg`]: one declared argument,
/// absent or null answering `None`, a STRING `Some` — every other type
/// the one typed rejection. The format's decode stays with its caller.
pub(crate) fn optional_string_arg(
    ctx: ExecCtx<'_>,
    row: &Row,
    source: &CallArgs,
    index: usize,
    arg_name: &str,
    name: &SmallName,
) -> Result<Option<String>> {
    match call_arg_value(&ctx, row, source, index, arg_name)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.to_string())),
        Some(_) => name.with_str(|name| {
            Err(Error::invalid_argument_type(format!(
                "{name} argument `{arg_name}` expects STRING"
            )))
        }),
    }
}

/// Resolve one declared CALL argument through the call's one passing-mode
/// value: the explicit positional expression evaluated against the driving
/// row, or the implicit parameter/row-column value. The one per-argument
/// resolver the generic binder and the `db.ontology.*` readers share, so
/// the policy is spelled once.
pub(crate) fn call_arg_value(
    ctx: &ExecCtx<'_>,
    row: &Row,
    source: &CallArgs,
    index: usize,
    arg_name: &str,
) -> Result<Option<Value>> {
    match source {
        CallArgs::Explicit(args) => args
            .get(index)
            .map(|expr| crate::eval::eval(ctx.graph, row, expr, ctx.params))
            .transpose(),
        CallArgs::Implicit => Ok(ctx
            .params
            .get(arg_name)
            .cloned()
            .or_else(|| row.get(arg_name).cloned())),
    }
}

/// Static shape of a `CALL` clause (name, arguments, yields, optionality).
#[derive(Clone, Copy)]
pub(crate) struct CallSpec<'a> {
    /// The clause's own name, carried as the clause carries it: rendered
    /// to text through `SmallName::with_str` at the handful of read sites
    /// (classification, contract errors), never allocated per clause and
    /// never held by a source — the binding owns its copy in the metadata.
    pub name: &'a SmallName,
    /// The call's one passing-mode value — the IR's owned [`CallArgs`],
    /// lowered once by the front end — borrowed for the call's run, so
    /// no consumer re-encodes the mode as a bool beside a bare slice.
    pub source: &'a CallArgs,
    pub yield_items: &'a YieldItems,
    pub optional: bool,
    pub where_clause: Option<&'a Expr>,
}

impl<'a> CallSpec<'a> {
    /// The one runtime `CALL` contract every dispatch path reads: the
    /// implicit-passing-mode backstop, then the arity, over the
    /// (required, total) pair the call's signature declares — the generic
    /// registry path passes its resolved pair, the diverted executors the
    /// classification's own signature's. Every path consults the same two
    /// predicates through this one method, never a hand-rebuilt arity half
    /// (stellar round 2, F-2-26). The verdict alone: paths that resolve
    /// arguments by name (the generic binder, the ontology executor) check
    /// this and never read a positional slice — the scans read theirs
    /// through [`Self::check_diverted_positional`].
    pub(crate) fn check_contract(&self, required: usize, total: usize) -> Result<()> {
        check_implicit_passing_mode(
            self.source.is_explicit(),
            required,
            self.yield_items.has_yield(),
        )?;
        if let CallArgs::Explicit(args) = self.source {
            self.name
                .with_str(|name| check_explicit_arity(name, required, total, args.len()))?;
        }
        Ok(())
    }

    /// The positional slice a value-taking executor reads: the explicit
    /// list, its length the contract validated — or `None` for the
    /// paren-less form, which by the grammar has no positional list at
    /// all, its arguments resolving by name (the generic binder's
    /// missing-name rejection; the facade states the same policy at
    /// compile time). `None` is the mode's own fact, never an empty
    /// slice a caller must re-interpret.
    pub(crate) fn positional(&self) -> Option<&'a [Expr]> {
        match self.source {
            CallArgs::Explicit(args) => Some(args),
            CallArgs::Implicit => None,
        }
    }

    /// The diverted-name contract — [`Self::check_contract`] over the
    /// classification's own signature (the one registry fetch
    /// [`divert_call`] already made). The `db.ontology.*` divert reads
    /// this at its resolution site: its executor resolves arguments
    /// by name, so the paren-less form carries no positional arity
    /// there (the runtime contract the generic path enforces, from the
    /// ONE registry signature).
    pub(crate) fn check_diverted_contract(&self, signature: &ProcSignature) -> Result<()> {
        self.check_contract(signature.required_args(), signature.args.len())
    }

    /// The diverted scan executors' contract — the vector and fulltext
    /// `db.index.*.query*` calls, whose scans read their arguments
    /// positionally and resolve no names: [`Self::check_diverted_contract`]'s
    /// pair plus the positional form itself, so a paren-less call —
    /// which passes no list — answers the arity rejection exactly like
    /// a short explicit one (`expected 3, got 0`), never a positional
    /// read off an empty slice (stellar round 2, F-2-43). Answers the
    /// validated positional slice.
    pub(crate) fn check_diverted_positional(
        &self,
        signature: &ProcSignature,
    ) -> Result<&'a [Expr]> {
        // The one pair derivation ([`Self::check_diverted_contract`]), then
        // the positional form itself: the implicit form is rejected here
        // directly, not by a fabricated arity — these scan executors read
        // their arguments positionally and resolve no names, so the
        // paren-less form can never supply them (the arity rejection over
        // the empty list the grammar passes, unconditionally).
        self.check_diverted_contract(signature)?;
        self.positional().ok_or_else(|| {
            self.name.with_str(|name| {
                Error::invalid_number_of_arguments(arity_detail(name, signature.args.len(), 0))
            })
        })
    }
}

/// The registry-driven explicit-arity check — one half of the runtime
/// `CALL` contract ([`CallSpec::check_contract`]), which every dispatch
/// path (the generic registry path, the ontology divert, the vector and
/// fulltext scans) reads: an explicit call passes between `required`
/// and `total` arguments. (The facade's compile-time `validate_calls`
/// already enforces the same contract.)
pub fn check_explicit_arity(name: &str, required: usize, total: usize, given: usize) -> Result<()> {
    if given < required || given > total {
        return Err(Error::invalid_number_of_arguments(arity_detail(
            name, total, given,
        )));
    }
    Ok(())
}

/// The one spelling of the arity rejection's detail — read by the explicit
/// check and by the diverted scans' paren-less rejection, so the two can
/// never drift.
fn arity_detail(name: &str, total: usize, given: usize) -> String {
    format!("{name} expected {total}, got {given}")
}

/// The passing-mode backstop — the other half of the runtime `CALL`
/// contract ([`CallSpec::check_contract`]): an in-query implicit
/// call — a `CALL` with a YIELD, in a clause pipeline — cannot pass
/// arguments; the procedure's arguments resolve only in a standalone unit
/// call. `required_args` is the caller's registry truth: the number of
/// declared parameters *without a default* (LABS-389: a procedure whose
/// parameters all carry defaults is callable implicitly). (The facade's
/// compile-time `validate_calls` enforces the same contract before
/// execution; this is the executor-side backstop.)
pub fn check_implicit_passing_mode(
    explicit: bool,
    required_args: usize,
    has_yield: bool,
) -> Result<()> {
    if !explicit && required_args > 0 && has_yield {
        return Err(Error::syntax(
            "InvalidArgumentPassingMode: in-query CALL cannot pass arguments implicitly",
        ));
    }
    Ok(())
}

/// The void-procedure policy in one spelling: an OPTIONAL call with no
/// return columns over no input rows pads one unit row (its own row,
/// answered as data); over rows, the per-row handler runs. The pad and
/// the handler are independent — the loop is empty-safe exactly when the
/// pad fires — so nothing guards one with the other.
pub(crate) fn pad_or_each(
    rows: &[Row],
    optional: bool,
    mut each: impl FnMut(&Row) -> Result<()>,
) -> Result<Option<Row>> {
    let pad = (rows.is_empty() && optional).then(Row::new);
    for row in rows {
        each(row)?;
    }
    Ok(pad)
}
