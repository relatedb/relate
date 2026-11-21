//! adoc walkthrough parsing (ADR-0144): extract the `[source,cypher]`
//! blocks from a `documentation/<name>.adoc` and classify each statement.

use relate_splitter::split_executable_statements;

use crate::diff::StatementOrder;
use crate::record::SkipReason;

/// One extracted Cypher block from the adoc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CypherBlock {
    /// 1-based adoc line where the block starts (the `[source,cypher]`
    /// attribute line) — the stable id for reports and skips.
    pub line: usize,
    /// The raw block body, statements unsplit.
    pub text: String,
}

/// Extract every `[source,cypher]` (also `[source,cypher,role=...]`) block.
/// Blocks tagged with a non-cypher language (`[source,console]`,
/// `[source,bash]`, …) are not Cypher walkthroughs and are skipped.
pub fn cypher_blocks(adoc: &str) -> Vec<CypherBlock> {
    let lines: Vec<&str> = adoc.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        // Asciidoc accepts the spaced attribute form (`[source, cypher]`)
        // as well as the compact one; normalize spaces out of the
        // attribute list before matching so neither form is dropped.
        let normalized: String = line
            .strip_prefix('[')
            .and_then(|l| l.strip_suffix(']'))
            .map(|attrs| format!("[{}]", attrs.replace(' ', "")))
            .unwrap_or_else(|| line.to_string());
        if normalized.starts_with("[source,cypher") {
            // find the delimiter (---- by default) then the closing delimiter
            if let Some((body, next)) = delimited_block(&lines, i + 1) {
                blocks.push(CypherBlock {
                    line: i + 1,
                    text: body,
                });
                i = next;
                continue;
            }
        }
        i += 1;
    }
    blocks
}

/// Read a delimited block starting at `from`: an opening delimiter line
/// (four or more `-`), the body, and the closing delimiter line. Returns
/// the joined body and the index of the first line after the block.
fn delimited_block(lines: &[&str], from: usize) -> Option<(String, usize)> {
    let mut i = from;
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    let opener = lines.get(i)?.trim();
    if !opener.starts_with("----") {
        return None;
    }
    let mut body = Vec::new();
    let mut j = i + 1;
    while j < lines.len() {
        if lines[j].trim().starts_with("----") {
            return Some((body.join("\n"), j + 1));
        }
        body.push(lines[j]);
        j += 1;
    }
    None // unterminated block: treat as prose, not Cypher
}

/// One walkthrough statement, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    /// Adoc block line (1-based) — the stable id for reports and skips.
    pub line: usize,
    /// Full statement text — the stable statement id hashes this.
    pub statement: String,
    /// The classification — the only per-variant data.
    pub kind: ClassifiedKind,
}

/// The classification kinds one statement can carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifiedKind {
    /// Runs differentially with exact row comparison.
    Plain,
    /// An apoc statement with the on-demand apoc flag on (ADR-0144): runs
    /// differentially like `Plain`; the kind itself is the apoc marker
    /// the capture consumes (F-1-10: apoc-ness is decided here, at
    /// classification time).
    Apoc,
    /// `LIMIT`/`SKIP` without `ORDER BY`: the engines may pick different
    /// rows, so the run compares against the reference's *full* result set
    /// (subset check) instead of the limited one.
    LimitedSelection,
    /// Skipped; the reason is reported, never silently dropped.
    Skipped(SkipReason),
}

/// Classify one statement by its leading token / procedure namespace.
/// `apoc_enabled` is the on-demand apoc flag (ADR-0144): with it, apoc
/// statements classify as runnable (`ClassifiedKind::Apoc`); without it
/// they skip with `SkipReason::Apoc`.
pub fn classify(line: usize, statement: &str, apoc_enabled: bool) -> Classified {
    let trimmed = statement.trim_start();
    // One normalized form feeds every check: string literals blanked
    // first (so a quoted `gds.`/`apoc.`/`limit 5` can not misroute the
    // classification), then lowercase with whitespace runs collapsed, so
    // multi-whitespace spellings (`USE\nneo4j`, `CREATE  INDEX`,
    // `LOAD\nCSV …`) classify the same as their single-space forms.
    // Collapsing is a no-op for the whitespace-free needles (`gds.`,
    // `apoc.`) and only widens the spaced prefixes in the safe skip
    // direction.
    let normalized = classification_form(trimmed);
    let kind = classified_kind(trimmed, &normalized, apoc_enabled);
    Classified::of_kind(line, statement, kind)
}

/// The classification rule chain (F-1-2): the only function that decides
/// a statement's [`ClassifiedKind`]. `trimmed` is the trimmed raw
/// statement (the URL probe needs the unblanked text); `normalized` is
/// the single normalized form `classify` computed — every rule matches on
/// that, so no rule re-normalizes.
fn classified_kind(trimmed: &str, normalized: &str, apoc_enabled: bool) -> ClassifiedKind {
    if contains_namespace(normalized, "gds.") {
        return ClassifiedKind::Skipped(SkipReason::Gds);
    }
    // apoc procedures/functions and the apocf function family — the same
    // token-bounded namespace match as gds, so an identifier like
    // `x.apoc.foo()` does not misroute to the Apoc skip.
    if contains_namespace(normalized, "apoc.") || contains_namespace(normalized, "apocf.") {
        return if apoc_enabled {
            // apoc flag on: runnable, marked apoc by the kind itself.
            ClassifiedKind::Apoc
        } else {
            ClassifiedKind::Skipped(SkipReason::Apoc)
        };
    }
    for (prefix, reason) in DIALECT_PREFIXES {
        if normalized.starts_with(prefix) {
            return ClassifiedKind::Skipped(SkipReason::Dialect(reason));
        }
    }
    // Walkthrough queries with `$param` placeholders take their values from
    // `:param` browser commands (skipped above); unbound, both engines
    // would fail on the missing value and the comparison says nothing.
    if references_parameter(trimmed) {
        return ClassifiedKind::Skipped(SkipReason::Dialect(
            "parameterized query; the adoc binds values via :param",
        ));
    }
    // Remote LOAD CSV is denied by the engine's LOAD CSV policy (ADR-0081):
    // local files only, under --load-csv-dir. The engine refuses by design,
    // so this is a policy skip, not a differential failure. Every skip rule
    // runs before the selection-mode classification below, so a remote
    // LOAD CSV with LIMIT lands here, never in LimitedSelection. The
    // keyword half is word-bounded like every sibling check and runs on
    // the literal-stripped form, so a quoted 'load csv' can not trigger
    // it; the URL half probes the raw statement, because the normalized
    // form has the URL string blanked.
    if contains_keyword(normalized, "load csv") && references_remote_url(trimmed) {
        return ClassifiedKind::Skipped(SkipReason::Dialect(
            "remote LOAD CSV is denied by the engine's LOAD CSV policy",
        ));
    }
    // A LIMIT/SKIP without ORDER BY selects an unspecified subset (or start
    // point); two engines may legitimately return different rows, so the
    // run checks every returned row against the reference's full result set
    // (subset check) instead of comparing the limited rows directly. The
    // whitespace-normalized match is the same one `is_ordered` uses, so
    // classification and row comparison never disagree on `ORDER\nBY` /
    // `ORDER  BY` spellings.
    if (contains_keyword(normalized, "limit") || contains_keyword(normalized, "skip"))
        && !order_by_present(normalized)
    {
        return ClassifiedKind::LimitedSelection;
    }
    ClassifiedKind::Plain
}

/// Does the statement carry an `ORDER BY`: [`StatementOrder::Ordered`]
/// (positional comparison) or [`StatementOrder::Unordered`] (bag)?
/// Shares the classifier's word-boundary matcher on the same
/// literal-stripped, whitespace-normalized form `classify` uses, so row
/// comparison and statement classification can never disagree:
/// `ORDER\nBY` and `ORDER  BY` detect, a word-boundary miss like
/// `reorder by` does not, and a quoted `'order by'` misleads neither.
pub fn is_ordered(statement: &str) -> StatementOrder {
    if order_by_present(&classification_form(statement)) {
        StatementOrder::Ordered
    } else {
        StatementOrder::Unordered
    }
}

/// `db.schema.visualization()` presence on a whitespace-normalized
/// lowercase statement — the single predicate `run_both` uses to route to
/// the visualization comparison ([`crate::diff::compare_rows_schema_viz`],
/// LABS-257). Shares `classification_form` with `is_ordered`, so statement
/// classification lives in this layer only.
pub fn is_schema_visualization(statement: &str) -> bool {
    // The canonical boundary-aware namespace matcher (the same left-edge-
    // bounded procedure check the apoc/gds skips use), not a bare
    // `contains` (F-5-1).
    contains_namespace(&classification_form(statement), "db.schema.visualization")
}

/// `ORDER BY` presence on a whitespace-normalized lowercase statement —
/// the single predicate `classify`'s limit/skip branch and `is_ordered`
/// both run, so statement classification and row comparison can never
/// disagree on whether a statement carries an `ORDER BY`.
fn order_by_present(normalized: &str) -> bool {
    contains_keyword(normalized, "order by")
}

/// The clause keywords [`strip_limit_skip`] removes — the one spelling of
/// the keyword set and its lengths (F-1-4 — one spelling of the clause
/// vocabulary), beside the limit/skip detection that routes the same
/// vocabulary to `LimitedSelection`.
const LIMIT_SKIP_KEYWORDS: [&str; 2] = ["LIMIT", "SKIP"];

/// Remove a trailing `LIMIT n` / `SKIP n` clause (case-insensitive, either
/// order) so the reference can return the full result set. Every statement
/// the classifier routes to `LimitedSelection` is a real query plus the
/// trailing clause, so stripping always yields a non-empty prefix (F-1-9).
pub(crate) fn strip_limit_skip(statement: &str) -> String {
    let mut out = statement
        .trim_end()
        .trim_end_matches(';')
        .trim_end()
        .to_string();
    loop {
        // A LIMIT/SKIP clause is `<keyword> <number>`, so the trailing token
        // is the number and the keyword sits just before it. The keyword
        // table is the one spelling of the clause keywords and their
        // lengths (F-1-4): find the matched keyword once, slice by its
        // own length. The number boundary is derived by trimming, not
        // index arithmetic (F-3-2): `rfind + 1` is a char boundary only
        // for a single-byte char, so a remainder ending in a multibyte
        // char would panic the slice; trimming stops at char edges.
        let number_start = out.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        if number_start >= out.len() {
            break; // no trailing number: no LIMIT/SKIP clause
        }
        let before = out[..number_start].trim_end();
        let upper = before.to_ascii_uppercase();
        let keyword = LIMIT_SKIP_KEYWORDS.iter().find(|kw| upper.ends_with(*kw));
        let Some(keyword) = keyword else {
            break; // trailing number with no LIMIT/SKIP keyword before it
        };
        out = before[..before.len() - keyword.len()]
            .trim_end()
            .to_string();
    }
    // A statement that strips to nothing is corpus or splitter drift:
    // no walkthrough statement is only a `LIMIT n`/`SKIP n` clause with no
    // query in front, so an empty result means the corpus or the splitter
    // changed shape — the always-on panic (F-2-4: one disposition for one
    // state, never a debug_assert) is the backstop that says so.
    if out.is_empty() {
        panic!("LIMIT/SKIP stripped to nothing (no query prefix): {statement:?}");
    }
    out
}

impl Classified {
    /// The one constructor (F-1-13): every `Classified` in the crate is
    /// built here — `classify` is the only caller, after the rule chain
    /// (`classified_kind`) has decided the kind. Full statement text
    /// (F-1-5): the doc promises the full statement and the stable id
    /// hashes it.
    fn of_kind(line: usize, statement: &str, kind: ClassifiedKind) -> Classified {
        Classified {
            line,
            statement: statement.to_string(),
            kind,
        }
    }

    /// The apoc marker (ADR-0144): every apoc-classified statement is
    /// marked, whether it ran under the on-demand flag
    /// (`ClassifiedKind::Apoc`) or was skipped without it
    /// (`ClassifiedKind::Skipped` with `SkipReason::Apoc`) — the local
    /// record the apoc branch consumes.
    pub fn is_apoc(&self) -> bool {
        matches!(
            self.kind,
            ClassifiedKind::Apoc | ClassifiedKind::Skipped(SkipReason::Apoc)
        )
    }

    /// Does this statement run on the engines? The single enumeration of
    /// the runnable kind set (F-8-5): every kind except `Skipped` runs.
    /// The suite's runnable-count guard goes through this predicate; the
    /// dispatch itself is a total match, so a new kind still fails to
    /// compile until both places are updated.
    pub fn is_runnable(&self) -> bool {
        !matches!(self.kind, ClassifiedKind::Skipped(_))
    }
}

/// Does the statement reference a `$name` parameter? String literals
/// (`'costs $10'`) are stripped first so a `$` inside quotes does not
/// false-skip the query.
fn references_parameter(statement: &str) -> bool {
    let without_strings = strip_string_literals(statement);
    let bytes = without_strings.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'$'
            && bytes
                .get(i + 1)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            return true;
        }
    }
    false
}

/// Replace single- and double-quoted string contents with blanks. Not a
/// full lexer: an unbalanced quote degenerates to blanking the rest, which
/// can only under-report parameters — the safe direction for a skip
/// classifier.
fn strip_string_literals(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' || c == '"' {
            let quote = c;
            out.push(' ');
            for c2 in chars.by_ref() {
                if c2 == quote {
                    break;
                }
            }
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// Does the statement pull CSV from a remote URL? Probes the raw
/// statement: the normalized classification form has string literals
/// blanked, so the URL is only visible here. Whitespace-normalized so
/// `FROM\n"http…"` spellings match; both quote styles are accepted,
/// since Cypher string literals may be single- or double-quoted.
fn references_remote_url(statement: &str) -> bool {
    let lower = whitespace_normalized_lower(statement);
    lower.contains("from \"http") || lower.contains("from 'http")
}

/// `gds.`/`apoc.` appear as procedure calls (`CALL gds.xxx`) or function
/// calls (`gds.xxx(...)`) — a namespace check anywhere in the statement,
/// so graph-function and procedure forms are both caught. The match is
/// token-bounded, so an identifier continuation like `n.gdsVersion` or
/// `x.apoc.` does not misroute the classification. String literals are
/// blanked before this check runs (see `classification_form`), so a
/// quoted "gds."/"apoc." mention never reaches the matcher.
fn contains_namespace(lower: &str, ns: &str) -> bool {
    lower.match_indices(ns).any(|(i, _)| {
        // The left edge must not be an identifier continuation
        // (`n.gdsVersion`, `x.apoc.`): the right edge is the call name.
        lower[..i]
            .bytes()
            .last()
            .is_none_or(|b| !b.is_ascii_alphanumeric() && b != b'_' && b != b'.')
    })
}

/// A whole-word keyword occurrence: both edges are non-identifier
/// (`skipper` must not read as SKIP, `unlimited` not as LIMIT).
fn contains_keyword(lower: &str, kw: &str) -> bool {
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    lower.match_indices(kw).any(|(i, _)| {
        let before_ok = lower[..i].bytes().last().is_none_or(|b| !ident(b));
        let after_ok = lower
            .as_bytes()
            .get(i + kw.len())
            .is_none_or(|&b| !ident(b));
        before_ok && after_ok
    })
}

/// The single normalized form every classification check matches on:
/// string literals blanked (`strip_string_literals`), then whitespace
/// runs collapsed and lowercased (`whitespace_normalized_lower`).
/// `classify` computes it once and `is_ordered` matches on the same
/// form, so statement classification and row comparison can never
/// disagree — and a quoted lookalike (`'gds.x'`, `'limit 5'`,
/// `'order by'`) can not misroute either.
fn classification_form(statement: &str) -> String {
    whitespace_normalized_lower(&strip_string_literals(statement))
}

/// Lowercase with every whitespace run collapsed to a single space, so a
/// multi-word keyword matches across newlines and double spaces
/// (`ORDER\nBY`, `ORDER  BY`). The building block of the single form
/// `classify` and `is_ordered` both match on (`classification_form`),
/// so the two can never disagree on whether a statement carries an
/// `ORDER BY`.
fn whitespace_normalized_lower(statement: &str) -> String {
    statement
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

const DIALECT_PREFIXES: &[(&str, &str)] = &[
    ("profile", "PROFILE plans differ between engines"),
    ("explain", "EXPLAIN plans differ between engines"),
    ("use ", "multi-database USE is out of the dialect"),
    ("show ", "SHOW administration is out of the dialect"),
    // `CREATE INDEX` / `CREATE CONSTRAINT` are NOT here: the engine runs
    // and enforces them (uniqueness raises `ConstraintValidationFailed`),
    // so the walkthroughs' schema DDL runs differentially like any other
    // statement — a rejected syntax still surfaces as a DIFF, not a skip.
    (":use", "browser command, not Cypher"),
    (":param", "browser command, not Cypher"),
    (":params", "browser command, not Cypher"),
];

/// Split an adoc block into statements and classify each. `apoc_enabled`
/// is the on-demand apoc flag (see [`classify`]).
pub fn classify_block(block: &CypherBlock, apoc_enabled: bool) -> Vec<Classified> {
    split_executable_statements(&block.text)
        .into_iter()
        .map(|s| classify(block.line, &s, apoc_enabled))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
Intro prose.

[source,cypher]
----
MATCH (m:Movie) RETURN m.title;
----

[source,console]
----
match (n) return n;
----

[source,cypher,role=required]
----
CALL gds.pageRank.stream('movies')
YIELD nodeId, score
RETURN nodeId;
----

[source,cypher]
----
CALL apoc.load.json($url) YIELD value
RETURN value;
----
"#;

    // F-1-3/F-1-4: the canonical classify-assertion plumbing — the
    // kind-match-with-panic-context and the expected-kind equality each
    // live in exactly one helper; the per-rule tests below are table rows
    // over them. Every helper classifies at line 1 with apoc off (the kind
    // is line- and flag-independent except where a helper takes the flag).

    /// The classified value carries exactly this kind (panic carries the
    /// full classified value).
    fn assert_kind_of(c: &Classified, expected: ClassifiedKind) {
        assert!(c.kind == expected, "{}: {c:?}", c.statement);
    }

    /// The statement classifies to exactly this skip (panic carries the
    /// full classified value).
    fn assert_skips(query: &str, reason: SkipReason) {
        let c = classify(1, query, false);
        assert_kind_of(&c, ClassifiedKind::Skipped(reason));
    }

    /// The statement classifies to a dialect skip whose reason mentions
    /// `marker` (panic carries the statement and the actual reason).
    fn assert_dialect_skip(query: &str, marker: &str) {
        match classify(1, query, false).kind {
            ClassifiedKind::Skipped(SkipReason::Dialect(why)) => {
                assert!(why.contains(marker), "{query}: {why}")
            }
            other => panic!("{query}: {other:?}"),
        }
    }

    /// The statement does NOT take this skip (the misroute guard; panic
    /// carries the full classified value).
    fn assert_not_skips(query: &str, reason: SkipReason) {
        let c = classify(1, query, false);
        assert!(
            !matches!(&c.kind, ClassifiedKind::Skipped(r) if *r == reason),
            "{query}: {c:?}"
        );
    }

    /// The statement classifies `Plain` (runs differentially, exact rows).
    fn assert_plain(query: &str) {
        assert_kind_of(&classify(1, query, false), ClassifiedKind::Plain);
    }

    /// The statement classifies `LimitedSelection` (subset check).
    fn assert_limited(query: &str) {
        assert_kind_of(&classify(1, query, false), ClassifiedKind::LimitedSelection);
    }

    #[test]
    fn extracts_only_cypher_blocks_with_line_ids() {
        let blocks = cypher_blocks(SAMPLE);
        assert_eq!(blocks.len(), 3, "console block excluded: {blocks:?}");
        assert_eq!(blocks[0].line, 4);
        assert!(blocks[0].text.contains("m.title"));
        assert!(blocks[2].text.contains("apoc.load.json"));
    }

    #[test]
    fn classifies_plain_gds_and_apoc() {
        let blocks = cypher_blocks(SAMPLE);
        let classified: Vec<Classified> = blocks
            .iter()
            .flat_map(|b| classify_block(b, false))
            .collect();
        assert_eq!(classified.len(), 3, "{classified:?}");
        assert_kind_of(&classified[0], ClassifiedKind::Plain);
        assert_kind_of(&classified[1], ClassifiedKind::Skipped(SkipReason::Gds));
        assert_kind_of(&classified[2], ClassifiedKind::Skipped(SkipReason::Apoc));
    }

    #[test]
    fn parameterized_walkthrough_queries_skip() {
        let blocks = cypher_blocks(SAMPLE);
        let sample = "MATCH (m:Movie {title: $title}) RETURN m";
        assert_dialect_skip(sample, "parameterized");
        // the sample's plain block (no $params) stays plain
        let plain = classify_block(&blocks[0], false);
        for c in &plain {
            assert_kind_of(c, ClassifiedKind::Plain);
        }
    }

    #[test]
    fn dialect_prefixes_skip_with_reasons() {
        for (query, marker) in [
            ("PROFILE MATCH (n) RETURN n", "PROFILE"),
            ("EXPLAIN MATCH (n) RETURN n", "EXPLAIN"),
            ("USE neo4j MATCH (n) RETURN n", "multi-database"),
            ("SHOW CONSTRAINTS", "administration"),
            (":param actor => 'Tom Hanks'", "browser command"),
            ("MATCH (m:Movie {title: $title}) RETURN m", "parameterized"),
        ] {
            assert_dialect_skip(query, marker);
        }
    }

    #[test]
    fn plain_queries_without_parameters_stay_plain() {
        assert_plain("MATCH (n:Movie) RETURN n.title");
    }

    #[test]
    fn unspecified_row_selection_gets_the_subset_check_and_remote_csv_skips() {
        // LIMIT/SKIP without ORDER BY: subset-check classification.
        for q in [
            "MATCH (m:Movie) WHERE m.released > 2000 RETURN m LIMIT 5",
            "MATCH (n) RETURN n SKIP 10",
        ] {
            assert_limited(q);
        }
        // LIMIT with ORDER BY stays plain (deterministic) — including the
        // multi-whitespace spellings, so classification agrees with
        // `is_ordered`.
        for q in [
            "MATCH (n) RETURN n ORDER BY n.name LIMIT 5",
            "MATCH (n) RETURN n ORDER\nBY n.name LIMIT 5",
            "MATCH (n) RETURN n ORDER  BY n.name LIMIT 5",
        ] {
            assert_plain(q);
        }
        // Remote LOAD CSV is a policy skip.
        assert_dialect_skip(
            "LOAD CSV WITH HEADERS FROM \"https://data.neo4j.com/northwind/products.csv\" AS row RETURN row",
            "LOAD CSV policy",
        );
    }

    #[test]
    fn multi_whitespace_spellings_classify_like_single_space() {
        // Every classify check matches on the whitespace-normalized form,
        // so newline/double-space spellings hit the same skips as their
        // single-space forms — none of these may classify Plain.
        for (query, marker) in [
            ("USE\nneo4j", "multi-database"),
            (
                "LOAD\nCSV WITH HEADERS FROM \"https://data.neo4j.com/northwind/products.csv\" AS row RETURN row",
                "LOAD CSV policy",
            ),
        ] {
            assert_dialect_skip(query, marker);
        }
        // Schema DDL (CREATE INDEX / CREATE CONSTRAINT) is NOT a skip: the
        // engine runs and enforces it, so multi-space spellings classify
        // Plain like everything else and run differentially.
        for q in [
            "CREATE  INDEX FOR (n:Movie) ON (n.title)",
            "CREATE\nCONSTRAINT FOR (n:Movie) REQUIRE n.title IS UNIQUE",
        ] {
            assert_plain(q);
        }
    }

    #[test]
    fn skip_rules_run_before_the_selection_mode_branch() {
        // A remote LOAD CSV with LIMIT must hit the policy skip, never
        // LimitedSelection — the latter would run the statement and record
        // the engine's policy denial (ADR-0081) as a differential failure.
        assert_dialect_skip(
            "LOAD CSV WITH HEADERS FROM \"https://data.neo4j.com/northwind/products.csv\" AS row RETURN row LIMIT 5",
            "LOAD CSV policy",
        );
    }

    #[test]
    fn remote_load_csv_skip_is_word_bounded_and_dual_quoted() {
        // Both string-literal quote styles are legal Cypher, so a
        // single-quoted remote URL must hit the same ADR-0081 policy skip
        // as the double-quoted form — otherwise the statement runs and the
        // engine's policy denial is recorded as a differential failure.
        for q in [
            "LOAD CSV WITH HEADERS FROM 'https://data.neo4j.com/northwind/products.csv' AS row RETURN row",
            "LOAD CSV WITH HEADERS FROM \"https://data.neo4j.com/northwind/products.csv\" AS row RETURN row",
        ] {
            assert_dialect_skip(q, "LOAD CSV policy");
        }
        // The words in a string literal without a remote URL must not hit
        // the skip: no `FROM 'http…"`/`FROM "http…"` follows them.
        assert_plain("RETURN 'load csv' AS s");
    }

    #[test]
    fn apoc_detection_is_token_bounded() {
        // `x.apoc.foo()`: the `apoc.` left edge is an identifier
        // continuation (`.`), so it must not misroute to the Apoc skip.
        assert_not_skips("x.apoc.foo()", SkipReason::Apoc);
        // A real apoc call still skips.
        assert_skips("CALL apoc.meta.stats()", SkipReason::Apoc);
    }

    /// F-1-10: apoc-ness is a classification-time decision — the flag
    /// decides the kind, and the kind (or the Apoc skip reason) is
    /// the marker the apoc capture consumes.
    #[test]
    fn apoc_classifies_as_runnable_under_the_flag() {
        // Flag on: runnable, and the kind itself is the apoc marker.
        let c = classify(1, "CALL apoc.meta.stats()", true);
        assert_kind_of(&c, ClassifiedKind::Apoc);
        assert!(c.is_apoc(), "{c:?}");
        // Flag off: the apoc skip, still marked.
        assert_skips("CALL apoc.meta.stats()", SkipReason::Apoc);
        assert!(classify(1, "CALL apoc.meta.stats()", false).is_apoc());
        // Non-apoc statements are never marked.
        assert!(!classify(1, "MATCH (n) RETURN n", true).is_apoc());
        assert!(!classify(1, "CALL gds.pageRank.stream('movies')", true).is_apoc());
    }

    #[test]
    fn string_literals_do_not_misroute_classification() {
        // A quoted namespace mention is prose, not a call: it must
        // classify Plain, not silently skip a runnable statement.
        for q in [
            "RETURN 'gds.graph.project' AS s",
            "RETURN 'apoc.meta.stats' AS s",
        ] {
            assert_plain(q);
        }
        // Real calls still skip.
        assert_skips(
            "CALL gds.graph.project('movies', 'Movie', '*')",
            SkipReason::Gds,
        );
        assert_skips("CALL apoc.meta.stats()", SkipReason::Apoc);
        // A LIMIT inside a string literal is not a row-selection limit.
        assert_plain("RETURN 'return limit 5' AS s");
        // `is_ordered` shares the literal-stripped form, so a quoted
        // 'order by' misleads neither classification nor row comparison.
        assert_eq!(
            is_ordered("RETURN 'order by' AS s"),
            StatementOrder::Unordered
        );
    }

    /// F-8-5: `is_runnable` is the single enumeration of the runnable
    /// kind set — every kind except `Skipped` runs.
    #[test]
    fn is_runnable_enumerates_the_runnable_variants() {
        let runnable = [
            classify(1, "MATCH (n) RETURN n", false),
            classify(1, "CALL apoc.meta.stats()", true),
            classify(1, "MATCH (n) RETURN n LIMIT 5", false),
        ];
        assert!(runnable.iter().all(Classified::is_runnable), "{runnable:?}");
        for skipped in [
            classify(1, "CALL gds.pageRank.stream('movies')", false),
            classify(1, "CALL apoc.meta.stats()", false),
            classify(1, "PROFILE MATCH (n) RETURN n", false),
        ] {
            assert!(!skipped.is_runnable(), "{skipped:?}");
        }
    }

    #[test]
    fn order_by_detection() {
        assert_eq!(
            is_ordered("MATCH (n) RETURN n ORDER BY n.name"),
            StatementOrder::Ordered
        );
        assert_eq!(is_ordered("MATCH (n) RETURN n"), StatementOrder::Unordered);
        // Multi-whitespace/newline spellings detect (formatted suite files).
        assert_eq!(
            is_ordered("MATCH (n)\nRETURN n\nORDER\nBY n.name"),
            StatementOrder::Ordered
        );
        assert_eq!(
            is_ordered("MATCH (n) RETURN n ORDER  BY n.name"),
            StatementOrder::Ordered
        );
        // A word-boundary miss does not detect.
        assert_eq!(
            is_ordered("MATCH (n) RETURN n reorder by n.name"),
            StatementOrder::Unordered
        );
    }

    #[test]
    fn strips_trailing_limit_and_skip() {
        assert_eq!(
            strip_limit_skip("MATCH (m:Movie) WHERE m.released > 2000 RETURN m LIMIT 5"),
            "MATCH (m:Movie) WHERE m.released > 2000 RETURN m"
        );
        assert_eq!(
            strip_limit_skip("MATCH (n) RETURN n ORDER BY n.name SKIP 10 LIMIT 5"),
            "MATCH (n) RETURN n ORDER BY n.name"
        );
        assert_eq!(
            strip_limit_skip("MATCH (n) RETURN n SKIP 10;"),
            "MATCH (n) RETURN n"
        );
        // No LIMIT/SKIP: unchanged.
        assert_eq!(strip_limit_skip("MATCH (n) RETURN n"), "MATCH (n) RETURN n");
    }

    /// F-3-2: a remainder whose last non-digit char is multibyte must not
    /// panic the number-boundary slice — the boundary is derived by
    /// trimming, never `rfind + 1` index arithmetic.
    #[test]
    fn strip_limit_skip_multibyte_tail_never_panics() {
        let stripped = strip_limit_skip("MATCH (n:Person {name: 'Müller'}) RETURN n.name LIMIT 5");
        assert_eq!(stripped, "MATCH (n:Person {name: 'Müller'}) RETURN n.name");
    }
}
