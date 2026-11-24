//! Relate parser front-end lowered to the Relate IR (ADR-0046).

pub mod agg;
pub(crate) mod ast;
mod call_scope;
mod ident_hash;
pub(crate) mod local_callables;
mod lower;
mod output_vars;
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
use lower::authored_column_label;
use relate_parser::BranchShape;
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
/// sites, ADR-0382), and lower into a planner-IR `Query`.
pub fn parse_and_lower(src: &str) -> Result<Query> {
    let mut stmt = relate_parser::parse(src).map_err(map_parse_error)?;
    // The ADR-0257 depth guard runs before the CIP-231 resolver walks the
    // tree: a too-deep body gets the typed depth error here, never a
    // stack overflow inside the resolver's own recursion.
    crate::validate::validate_nesting_depth(&stmt)
        .map_err(|error| Error::syntax(error.to_string()))?;
    local_callables::resolve(&mut stmt)?;
    lower_statement(&stmt)
}

/// The resolve-then-lower tail every top-level entry shares: the CIP-231
/// rewrite pass (ADR-0382) eliminates `DEFINE` blocks and their call
/// sites, then lowering runs on the plain pre-existing Cypher that
/// remains, keeping the statement's own `EXPLAIN`/`PROFILE` preamble
/// mode. `params` are the statement's bound parameters, which the
/// dynamic-operand admission rule treats as compile-time constants
/// (LABS-709).
/// `lex_parse` is the caller's already-closed parse window (tokenize plus
/// the AST build — the one meaning, measured at the parse call in every
/// entry, never re-closed here), so the PARSE stage's fine sub-split
/// (LABS-1120, [`ParseStages`]) is stamped at the boundaries this tail
/// owns.
fn resolve_and_lower(
    mut stmt: relate_parser::Statement,
    lex_parse: Duration,
    options: validate::ValidateOptions,
    policy: StatementPolicy,
    params: &std::collections::BTreeMap<String, relate_core::Value>,
) -> Result<LoweredStatement> {
    let rewrite_started = Instant::now();
    crate::validate::validate_nesting_depth(&stmt)
        .map_err(|error| Error::syntax(error.to_string()))?;
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
    // ADR-0257: enforce the query-size limit before parsing. The parser
    // enforces the same cap; this check keeps the error on the front-end
    // path even if a caller reaches lowering another way. LABS-1120: the
    // lex_parse sub-window is the parse call's own window, closed at the
    // call — the one meaning every entry stamps.
    let parse_started = Instant::now();
    let stmt = parse_sized(src)?;
    let lex_parse = parse_started.elapsed();
    resolve_and_lower(
        stmt,
        lex_parse,
        validate::ValidateOptions::default(),
        StatementPolicy::default(),
        &std::collections::BTreeMap::new(),
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
    )
}

/// [`parse_and_lower_statement_with_policy`] with the statement's bound
/// parameter values: the engine's admission path, which has them and must
/// pass them so a parameter-typed dynamic operand of the wrong type fails
/// before execution (LABS-709) instead of hiding behind a zero-row `MATCH`.
pub fn parse_and_lower_statement_with_policy_and_params(
    src: &str,
    preview: relate_core::PreviewFeatures,
    policy: StatementPolicy,
    params: &std::collections::BTreeMap<String, relate_core::Value>,
) -> Result<LoweredStatement> {
    // Plan-cache placeholders are the literals they replaced. Compile-time
    // literal checks (boolean operands, SKIP/LIMIT, variable types) read
    // them for this parse only.
    let _auto = relate_core::AutoLiteralGuard::install(params);
    // LABS-1120: the lex_parse sub-window is the parse call's own window,
    // closed at the call — before the preview-gate walk below, which stays
    // in the coarse PARSE window alone (an inter-stage gap, like the
    // engine's post-EXPLAIN gates).
    let parse_started = Instant::now();
    // The `mixed-label-syntax` gate widens what the parser accepts (the
    // floor's CIP-40/CIP-14 mixing rejections lift), so it threads into
    // the parse itself as `ParseOptions` -- unlike the AST-detector gates
    // below, there is no floor AST to detect the construct in.
    let stmt = parse_sized_with_options(src, parse_options(preview))?;
    let lex_parse = parse_started.elapsed();
    // The one gate loop, derived from the registry itself: every row's
    // detector is consulted here, so a detector-less registry row is a
    // compile error. The graph-type family's AST walk runs once, before the
    // loop, and its arms report membership instead of a second loop.
    let graph_type_gates =
        validate::statement_graph_type_gates(&stmt).map_err(local_callables::walker_error)?;
    for feature in relate_core::PREVIEW_FEATURES {
        for site in feature.gate_sites() {
            let relate_core::GateSite::AstDetector(kind) = site else {
                continue;
            };
            let uses = detector_uses(*kind, *feature, &stmt, &graph_type_gates)
                .map_err(local_callables::walker_error)?;
            if uses {
                preview.require(*feature)?;
            }
        }
    }
    // The constructs that only materialize against stored graph-type state
    // fail closed in the executor's graph-type layer with the same authored
    // error.
    resolve_and_lower(
        stmt,
        lex_parse,
        validate::ValidateOptions { preview },
        policy,
        params,
    )
}

/// The shared size-capped parse of [`parse_and_lower_statement`] and
/// [`parse_and_lower_statement_with_preview`]: ADR-0079's query-size limit
/// before parsing (the parser enforces the same cap; this check keeps the
/// error on the front-end path even if a caller reaches lowering another
/// way), then the parse itself. Every gate closed.
fn parse_sized(src: &str) -> Result<relate_parser::Statement> {
    parse_sized_with_options(src, relate_parser::lexer::ParseOptions::default())
}

/// Switch on the `ParseOptions` field a parse-site feature names. Total over
/// [`relate_core::ParseOption`]: a new option must say which field it sets.
fn apply_parse_option(
    option: relate_core::ParseOption,
    options: &mut relate_parser::lexer::ParseOptions,
) {
    use relate_core::ParseOption as O;
    match option {
        O::ObfuscatedLiterals => options.obfuscated_literals = true,
        O::MixedLabelSyntax => options.mixed_label_syntax = true,
        O::CypherAnnotations => options.cypher_annotations = true,
        O::PathConstruction => options.path_construction = true,
    }
}

/// Run the AST detector a feature's `AstDetector` site names. Total over
/// [`relate_core::DetectorKind`]; the graph-type family reads the membership
/// its single walk already computed.
fn detector_uses(
    kind: relate_core::DetectorKind,
    feature: relate_core::PreviewFeature,
    stmt: &relate_parser::Statement,
    graph_type_gates: &[relate_core::PreviewFeature],
) -> std::result::Result<bool, relate_error::EngineError> {
    use relate_core::DetectorKind as D;
    match kind {
        D::HybridSearch => validate::statement_uses_hybrid_search(stmt),
        D::LocalCallables => local_callables::statement_uses_local_callables(stmt),
        D::Float16 => validate::statement_uses_float16_coordinate_types(stmt),
        D::PathConstruction => validate::statement_uses_path_construction(stmt),
        D::Chronological => validate::statement_uses_chronological_comparisons(stmt),
        D::GraphTypeMembership => Ok(graph_type_gates.contains(&feature)),
    }
}

/// The parse-affecting options derived from the store's preview set: each
/// enabled feature's `ParseOption` sites set their `ParseOptions` field.
fn parse_options(preview: relate_core::PreviewFeatures) -> relate_parser::lexer::ParseOptions {
    let mut options = relate_parser::lexer::ParseOptions::default();
    for feature in relate_core::PREVIEW_FEATURES {
        if !preview.enabled(*feature) {
            continue;
        }
        for site in feature.gate_sites() {
            if let relate_core::GateSite::ParseOption(option) = site {
                apply_parse_option(*option, &mut options);
            }
        }
    }
    options
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

/// The authored column names of a statement's final explicit `RETURN`,
/// parsed under the given preview-derived options — the facade's
/// authored-name restore reads these for columns the compiled plan
/// spelled as auto-extraction placeholders (ADR-0663's trigger
/// alignment): the same options, the same grammar, as the statement's
/// own admission parse, and no raw parser AST crosses the crate
/// boundary. `None` when the projection is any other shape (a UNION
/// head, `RETURN *`, a conditional's body, a trailing `CALL`) — the
/// parser's one projection model decides.
pub fn authored_column_names(
    src: &str,
    preview: relate_core::PreviewFeatures,
) -> Result<Option<Vec<String>>> {
    let statement = parse_sized_with_options(src, parse_options(preview))?;
    Ok(match statement.projection_shape() {
        // A plain-items projection: the authored name per item — the
        // alias, else the label `column_label`'s parser-level twin
        // derives (the CIP-128 display rewrite included), else the
        // implicit name. A parsed item always carries one of the three
        // (`ReturnItem::output_name`'s own contract), so the per-item
        // answer is total.
        BranchShape::Items(items) => Some(
            items
                .iter()
                .map(|item| {
                    item.alias
                        .as_ref()
                        .map(|alias| alias.to_string())
                        .or_else(|| authored_column_label(&item.expr, &item.source_text))
                        .or_else(|| item.implicit_name().map(|name| name.to_string()))
                        .expect("a parsed return item always has an output name")
                })
                .collect(),
        ),
        // Every other projection shape — `RETURN *`, a trailing
        // column-exporting `CALL`, no result at all: no plain items to
        // name, and the restore keeps the plan's spelling.
        BranchShape::Wildcard | BranchShape::Omitted => None,
    })
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
