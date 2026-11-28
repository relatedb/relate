//! Relate parser front-end lowered to the Relate IR (ADR-0046).

pub mod agg;
pub(crate) mod ast;
mod call_scope;
pub mod compat_callables;
mod ident_hash;
pub(crate) mod local_callables;
mod lower;
mod output_vars;
mod preview_gate;
pub mod show_columns;
pub mod validate;

use std::time::Duration;

use relate_core::{Error, Result};
use relate_ir::Query;
use web_time::Instant;

pub use lower::{
    lower_expr, lower_order, lower_statement, lower_statement_with_limits, lower_type_expr,
};
pub use validate::{is_known_function, KNOWN_FUNCTIONS};

// `StatementPolicy` is reached through its home module, `validate`, so the
// type has one public path rather than a second re-export alias (LABS-311).
use preview_gate::parse_options;
use validate::StatementPolicy;

/// A parsed and lowered top-level statement: the planner input plus the
/// statement's `EXPLAIN`/`PROFILE` preamble mode.
///
/// Only the top-level statement carries the mode (the preamble grammar is
/// top-level only); nested bodies (`CALL { }`, `NEXT` stages, expression
/// subqueries) always report
/// [`PreambleMode::Execute`](relate_parser::PreambleMode::Execute). The
/// mode is deliberately *not* part of [`Query`]: honoring it happens above
/// planning, at the connection layer, so no execution path can mistake an
/// `EXPLAIN` for its wrapped statement.
pub struct LoweredStatement {
    pub query: Query,
    pub preamble: relate_parser::PreambleMode,
    /// The PARSE stage's fine sub-split (LABS-1120), stamped always-on by
    /// the entry that lowered the statement. Zero on every field only when
    /// nothing ran there — there is no second spelling of the windows.
    pub stages: ParseStages,
}

/// The PARSE stage's always-on fine sub-split (LABS-1120): three sub-windows
/// of the admission's PARSE duration, so their sum never exceeds it. The
/// boundaries are the ones the front actually has — two requested splits are
/// not cheaply obtainable and stay fused, each for an architectural reason:
/// the **lexer and the parser** are fused at `Parser::new`, the parser's one
/// tokenizing site (pre-tokenizing externally to time the lexer alone would
/// run the tokenizer twice — a real per-statement tax); and **validation and
/// lowering** are fused because the validation battery runs inside the
/// lowering walk, so a per-pass split would restructure `lower`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseStages {
    /// Lexing plus the AST build: the parse call's own window, closed at
    /// the call in every entry — ONE meaning (the preview-gate walk that
    /// follows the parse in the gated entry is not part of it; it stays in
    /// the coarse PARSE window alone, an inter-stage gap like the engine's
    /// post-EXPLAIN gates).
    pub lex_parse: Duration,
    /// The rewrite pass — the ADR-0382 CIP-231 `DEFINE`-block resolver,
    /// with the ADR-0257 nesting-depth guard that precedes it.
    pub rewrite: Duration,
    /// Validation and lowering (fused): the lowering battery's window,
    /// `validate_nesting_depth`'s semantics included in the battery.
    pub lower: Duration,
}

impl ParseStages {
    /// The zero split: no sub-window ran. The plan-cache hit path's value —
    /// nothing was parsed (the hit's outcome explains why), so the fine
    /// windows are true zeros, never unstamped placeholders.
    pub const ZERO: ParseStages = ParseStages {
        lex_parse: Duration::ZERO,
        rewrite: Duration::ZERO,
        lower: Duration::ZERO,
    };
}

/// Maps a `relate-parser` failure to a front-end error.
///
/// ADR-0116 §2: a parse failure tagged
/// [`UNSUPPORTED_ADMINISTRATION_DETAIL`](relate_parser::parser::UNSUPPORTED_ADMINISTRATION_DETAIL)
/// is a recognized database administration command outside CE's supported
/// set; its message is the verbatim statement text (without a leading
/// `CYPHER n`), echoed in CE's `UnsupportedAdministrationCommand` (51N27)
/// *semantic* error. Every other parse failure stays a syntax error,
/// prepending the TCK error-detail tag when one is present.
fn map_parse_error(e: relate_parser::ParseError) -> Error {
    if e.detail == Some(relate_parser::parser::UNSUPPORTED_ADMINISTRATION_DETAIL) {
        return relate_core::ce_messages::unsupported_administration_command(&e.message);
    }
    if let Some(detail) = e.detail {
        Error::syntax(format!("{detail}: {e}"))
    } else {
        Error::syntax(e.to_string())
    }
}

/// Parse with `relate-parser`, resolve CIP-231 local callables away
/// (the rewrite pass that eliminates `DEFINE` blocks and their call
/// sites, ADR-0382), and lower into a planner-IR `Query`. Rides the same
/// resolve-and-lower tail as every statement entry, so compat-callables
/// injection (ADR-0699) applies here too.
pub fn parse_and_lower(src: &str) -> Result<Query> {
    Ok(parse_and_lower_statement(src)?.query)
}

/// The storeless entries' injection predicate (ADR-0699): no store, so
/// no store-registered procedure can be shadowed and every compat table
/// name stays injectable — one named fact for the storeless call sites,
/// in place of the same closure literal re-spelled per entry.
fn storeless_procedure(_: &str) -> bool {
    false
}

/// The resolve-then-lower tail every top-level entry shares: the
/// compat-callables injection (ADR-0699) and the CIP-231 rewrite pass
/// (ADR-0382) eliminate foreign procedure calls and `DEFINE` blocks with
/// their call sites, then lowering runs on the plain pre-existing Cypher
/// that remains, keeping the statement's own `EXPLAIN`/`PROFILE` preamble
/// mode. `params` are the statement's bound parameters, which the
/// dynamic-operand admission rule treats as compile-time constants
/// (LABS-709).
/// `lex_parse` is the caller's already-closed parse window (tokenize plus
/// the AST build — the one meaning, measured at the parse call in every
/// entry, never re-closed here), so the PARSE stage's fine sub-split
/// (LABS-1120, [`ParseStages`]) is stamped at the boundaries this tail
/// owns.
/// `store_has_procedure` answers whether the store has registered the
/// name — `true` declines the compat-callables table entry for it
/// (ADR-0699 precedence); storeless entries pass
/// [`storeless_procedure`].
fn resolve_and_lower(
    mut stmt: relate_parser::Statement,
    lex_parse: Duration,
    options: validate::ValidateOptions,
    policy: StatementPolicy,
    params: &std::collections::BTreeMap<String, relate_core::Value>,
    store_has_procedure: &dyn Fn(&str) -> bool,
) -> Result<LoweredStatement> {
    let rewrite_started = Instant::now();
    crate::validate::validate_nesting_depth(&stmt).map_err(ast::tree::walker_error)?;
    // ADR-0699: the compat-callables injection runs in this shared tail,
    // so no parse entry can omit it — an injected definition is
    // engine-provided, not a user `DEFINE`, and in the gated entries the
    // preview gate has already run on the user's AST before this tail, so
    // an injected definition never trips the `local-callables` gate.
    compat_callables::inject(&mut stmt, store_has_procedure)?;
    local_callables::resolve(&mut stmt)?;
    let rewrite = rewrite_started.elapsed();
    let lower_started = Instant::now();
    let query = lower::lower_statement_with_options_and_policy(&stmt, options, policy, params)?;
    let lower = lower_started.elapsed();
    Ok(LoweredStatement {
        preamble: stmt.preamble,
        query,
        stages: ParseStages {
            lex_parse,
            rewrite,
            lower,
        },
    })
}

/// Parse and lower a top-level statement, keeping its `EXPLAIN`/`PROFILE`
/// preamble mode alongside the lowered query.
pub fn parse_and_lower_statement(src: &str) -> Result<LoweredStatement> {
    // The size-capped, timed parse — [`parse_timed`] owns the window's
    // definition and the cap's rationale.
    let (stmt, lex_parse) = parse_timed(src, relate_parser::lexer::ParseOptions::default())?;
    resolve_and_lower(
        stmt,
        lex_parse,
        validate::ValidateOptions::default(),
        StatementPolicy::default(),
        &std::collections::BTreeMap::new(),
        &storeless_procedure,
    )
}

/// Parse and lower a top-level statement behind the ADR-0371 preview gate:
/// a statement whose AST uses a gated preview construct fails closed with
/// the authored disabled-feature error unless `preview` enables it, before
/// anything of it lowers or plans. This is the preview-gated, default-policy
/// convenience; the engine's statement admission goes through
/// [`parse_and_lower_statement_with_policy`] with the store's resolved
/// preview set and policy. The ungated [`parse_and_lower_statement`] stays
/// the dialect-floor entry for callers without a store.
pub fn parse_and_lower_statement_with_preview(
    src: &str,
    preview: relate_core::PreviewFeatures,
) -> Result<LoweredStatement> {
    parse_and_lower_statement_with_policy(src, preview, StatementPolicy::default())
}

/// [`parse_and_lower_statement_with_preview`] with the store's resolved
/// traversal-depth admission limit (LABS-610). The safe-off convenience
/// retained for callers that have a limit but no full [`StatementPolicy`];
/// the engine's admission goes through
/// [`parse_and_lower_statement_with_policy`]. The other entries delegate
/// with [`relate_core::MAX_TRAVERSAL_DEPTH`].
pub fn parse_and_lower_statement_with_limits(
    src: &str,
    preview: relate_core::PreviewFeatures,
    max_traversal_depth: u32,
) -> Result<LoweredStatement> {
    parse_and_lower_statement_with_policy(
        src,
        preview,
        StatementPolicy::unsafened(max_traversal_depth),
    )
}

/// [`parse_and_lower_statement_with_limits`] plus the opt-in safe-mode shape
/// rules (LABS-680, ADR-0477): the engine's admission passes the store's
/// resolved `OpenOptions::safe_mode` and traversal limit in one
/// [`StatementPolicy`]. Every other entry delegates with safe mode off.
pub fn parse_and_lower_statement_with_policy(
    src: &str,
    preview: relate_core::PreviewFeatures,
    policy: StatementPolicy,
) -> Result<LoweredStatement> {
    parse_and_lower_statement_with_policy_and_params(
        src,
        preview,
        policy,
        &std::collections::BTreeMap::new(),
        &storeless_procedure,
    )
}

/// [`parse_and_lower_statement_with_policy`] with the statement's bound
/// parameter values and the store's procedure-registry predicate: the
/// engine's admission path, which has them and must pass them so a
/// parameter-typed dynamic operand of the wrong type fails before
/// execution (LABS-709) instead of hiding behind a zero-row `MATCH`, and
/// passes `store_has_procedure` so a compat-callables injection
/// (ADR-0699, [`compat_callables`]) never shadows a procedure the store
/// registered. The predicate answers whether the store HAS the name —
/// `true` declines the table entry for it; `false` (also the storeless
/// entries' constant answer) leaves it injectable.
pub fn parse_and_lower_statement_with_policy_and_params(
    src: &str,
    preview: relate_core::PreviewFeatures,
    policy: StatementPolicy,
    params: &std::collections::BTreeMap<String, relate_core::Value>,
    store_has_procedure: &dyn Fn(&str) -> bool,
) -> Result<LoweredStatement> {
    // Plan-cache placeholders are the literals they replaced. Compile-time
    // literal checks (boolean operands, SKIP/LIMIT, variable types) read
    // them for this parse only.
    let _auto = relate_core::AutoLiteralGuard::install(params);
    // The preview-gate walk below stays in the coarse PARSE window alone
    // (an inter-stage gap, like the engine's post-EXPLAIN gates), after
    // [`parse_timed`]'s window has closed.
    // The `mixed-label-syntax` gate widens what the parser accepts (the
    // floor's CIP-40/CIP-14 mixing rejections lift), so it threads into
    // the parse itself as `ParseOptions` -- unlike the AST-detector gates
    // below, there is no floor AST to detect the construct in.
    let (stmt, lex_parse) = parse_timed(src, parse_options(preview))?;
    // The preview gate runs on the user's AST (ADR-0371): a gated preview
    // construct fails closed with the authored disabled-feature error
    // before anything of it lowers or plans.
    preview_gate::require_detected_gates(&stmt, preview)?;
    // The compat-callables injection then happens inside the shared
    // resolve-and-lower tail below — an injected definition is
    // engine-provided, not a user `DEFINE`, so it cannot trip the
    // `local-callables` gate that already ran. The constructs that only
    // materialize against stored graph-type state fail closed in the
    // executor's graph-type layer with the same authored error.
    resolve_and_lower(
        stmt,
        lex_parse,
        validate::ValidateOptions { preview },
        policy,
        params,
        store_has_procedure,
    )
}

/// The shared size-capped, timed parse every statement entry stamps:
/// ADR-0079's query-size limit before parsing (the parser enforces the
/// same cap; this check keeps the error on the front-end path even if a
/// caller reaches lowering another way), then the parse itself, closing
/// the lex_parse window — the parse call's own window, the one meaning
/// every entry stamps (LABS-1120) — so the window's definition has one
/// home instead of a copy per entry. Every gate closed.
fn parse_timed(
    src: &str,
    options: relate_parser::lexer::ParseOptions,
) -> Result<(relate_parser::Statement, Duration)> {
    let parse_started = Instant::now();
    let stmt = parse_sized_with_options(src, options)?;
    Ok((stmt, parse_started.elapsed()))
}

/// Normalize a statement for the plan cache (ADR-0503): the engine
/// admission path's one entry into the parser's token-stream normalizer,
/// under the same preview-derived [`ParseOptions`] the real parse would
/// use (the gates change what the lexer produces, so the template must
/// come from the identical token stream). A pure mapping — cache
/// participation is decided where it must be, in the cache's own lookup
/// before any stage runs; a statement *referencing* an `__AUTO_*` name
/// still bails inside the normalizer, for the same reserved-name reason.
pub fn normalize_statement(
    src: &str,
    preview: relate_core::PreviewFeatures,
) -> relate_parser::NormalizeOutcome {
    relate_parser::normalize_statement(src, parse_options(preview))
}

/// [`parse_sized`] with explicit parse-affecting options.
fn parse_sized_with_options(
    src: &str,
    options: relate_parser::lexer::ParseOptions,
) -> Result<relate_parser::Statement> {
    if src.len() > relate_parser::parser::MAX_QUERY_BYTES {
        return Err(Error::syntax(format!(
            "query text of {} bytes exceeds the maximum query size of {} bytes",
            src.len(),
            relate_parser::parser::MAX_QUERY_BYTES
        )));
    }
    relate_parser::parse_with_options(src, options).map_err(map_parse_error)
}

/// Parse Cypher for `Database::query`.
///
/// Relate is the product front-end: `relate-parser` lowered into
/// planner IR (ADR-0046, ADR-0277).
///
/// Callers that honor the `EXPLAIN`/`PROFILE` prefixes (the connection
/// layer) use [`parse_and_lower_statement`]; this wrapper drops the flags.
pub fn parse_cypher(src: &str) -> Result<Query> {
    Ok(parse_and_lower_statement(src)?.query)
}

#[cfg(test)]
mod tests;
