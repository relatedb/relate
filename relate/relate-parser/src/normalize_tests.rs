//! Normalizer units (ADR-0503): the template key, the guards, the
//! rewritten text, and the not-cacheable bailouts.

use super::*;
use NormalizedValue::Int;

/// Normalize with every preview gate closed (the dialect floor).
fn norm(src: &str) -> NormalizeOutcome {
    normalize_statement(src, ParseOptions::default())
}

fn normalized(src: &str) -> Normalized {
    match norm(src) {
        NormalizeOutcome::Normalized(n) => n,
        NormalizeOutcome::NotCacheable => panic!("expected {src:?} to normalize"),
    }
}

fn template(src: &str) -> String {
    normalized(src).template
}

fn values(src: &str) -> Vec<(String, NormalizedValue)> {
    normalized(src).values
}

/// Keyword case is deliberately NOT folded: the dialect keeps keyword
/// spellings as case-sensitive names — `(o:Order)` parses with the name
/// as written (ADR-0169), `n.match` binds a property by spelling, a map
/// key `match` likewise — and the token stream cannot prove a keyword
/// sits in a clause position rather than a name position. So every
/// keyword renders as written and case-distinct spellings keep distinct
/// templates: the safe direction (a miss, never a wrong plan). A variable
/// rename never shares either (no variable renaming).
#[test]
fn keyword_case_stays_as_written() {
    // The review-found collisions: folding these would serve one
    // spelling's plan for the other's label, property key or map key.
    assert_ne!(
        template("MATCH (n:order) RETURN n"),
        template("MATCH (n:Order) RETURN n")
    );
    assert_ne!(template("RETURN n.match"), template("RETURN n.MATCH"));
    assert_ne!(template("RETURN {match: 1}"), template("RETURN {MATCH: 1}"));
    // Statement-level keyword case also stays — a miss, never a wrong key.
    assert_ne!(
        template("match (a:Person) where a.age > 21 with a limit 5"),
        template("MATCH (a:Person) WHERE a.age > 21 WITH a LIMIT 5")
    );
    // A rename is structural: different template.
    assert_ne!(
        template("MATCH (a:Person) RETURN a"),
        template("MATCH (b:Person) RETURN b")
    );
    // Non-clause keywords stay as written: they are legal identifiers
    // whose spelling is case-sensitive.
    assert_ne!(template("RETURN count"), template("RETURN COUNT"));
    assert_ne!(template("RETURN n.true"), template("RETURN n.TRUE"));
}

/// Value-bearing literals become typed placeholders; the type is part of
/// the key, so `42`, `'42'` and `4.2` never share a template, while
/// different values of one type do.
#[test]
fn typed_literal_placeholders() {
    assert_eq!(
        template("MATCH (n:User {id: 42}) RETURN n"),
        "MATCH ( n : User { id : $__AUTO_INT_0__ } ) RETURN n"
    );
    assert_eq!(
        template("MATCH (n:User {id: 43}) RETURN n"),
        template("MATCH (n:User {id: 42}) RETURN n")
    );
    let int = template("RETURN 42");
    let float = template("RETURN 4.2");
    let string = template("RETURN '42'");
    assert_ne!(int, float);
    assert_ne!(int, string);
    assert_ne!(float, string);
    assert_eq!(
        values("RETURN 'hello'"),
        vec![(
            "__AUTO_STR_0__".into(),
            NormalizedValue::Str("hello".into())
        )]
    );
}

/// A bracket group of only normalizable literals — recursively — collapses
/// to one list parameter; anything else inside keeps the bracket and
/// normalizes element-wise.
#[test]
fn lists_collapse_to_one_param() {
    assert_eq!(template("RETURN [1, 'a', 2.5]"), "RETURN $__AUTO_LIST_0__");
    assert_eq!(
        values("RETURN [1, 'a', 2.5]"),
        vec![(
            "__AUTO_LIST_0__".into(),
            NormalizedValue::List(vec![
                NormalizedValue::Int(1),
                NormalizedValue::Str("a".into()),
                NormalizedValue::Float(2.5),
            ])
        )]
    );
    assert_eq!(template("RETURN []"), "RETURN $__AUTO_LIST_0__");
    assert_eq!(template("RETURN [[1, 2], [3]]"), "RETURN $__AUTO_LIST_0__");
    assert_eq!(
        values("RETURN [[1, 2], [3]]"),
        vec![(
            "__AUTO_LIST_0__".into(),
            NormalizedValue::List(vec![
                NormalizedValue::List(vec![Int(1), Int(2)]),
                NormalizedValue::List(vec![Int(3)]),
            ])
        )]
    );
    // An expression element keeps the bracket; its literal elements take
    // their own placeholders.
    assert_eq!(
        template("RETURN [1, 2 + 3]"),
        "RETURN [ $__AUTO_INT_0__ , $__AUTO_INT_1__ + $__AUTO_INT_2__ ]"
    );
}

/// A string followed by `:` is a map-literal key: parameters are not map
/// keys and the key is structural, so it stays in the template (quoted);
/// the value position still normalizes.
#[test]
fn map_key_strings_stay() {
    assert_eq!(
        template("RETURN {'a': 1}"),
        "RETURN { 'a' : $__AUTO_INT_0__ }"
    );
    // Different keys, different templates — never a shared plan.
    assert_ne!(template("RETURN {'a': 1}"), template("RETURN {'b': 1}"));
    // The quoted and identifier spellings of one key keep distinct
    // templates (a miss, never a wrong key).
    assert_ne!(template("RETURN {'a': 1}"), template("RETURN {a: 1}"));
}

/// LOAD CSV's `FIELDTERMINATOR` requires a literal string; the URL, an
/// ordinary expression, still normalizes.
#[test]
fn load_csv_field_terminator_stays() {
    let t = template("LOAD CSV WITH HEADERS FROM 'data.csv' AS row FIELDTERMINATOR ';' RETURN row");
    assert!(t.contains("$__AUTO_STR_0__"), "the URL normalizes: {t}");
    assert!(t.contains("';'"), "the terminator stays: {t}");
    assert_ne!(
        template("LOAD CSV FROM 'a.csv' AS r FIELDTERMINATOR ';' RETURN r"),
        template("LOAD CSV FROM 'a.csv' AS r FIELDTERMINATOR ',' RETURN r")
    );
}

/// Var-length quantifier bounds take literal integers only, so an
/// `Integer`/`Float` directly after `*` or `..` stays in the template.
#[test]
fn quantifier_bounds_stay() {
    let t = template("MATCH p = (a)-[:KNOWS*1..3]->(b) RETURN p");
    assert!(t.contains("* 1 .. 3"), "the bounds stay: {t}");
    assert_ne!(
        template("MATCH p = (a)-[:KNOWS*1..3]->(b) RETURN p"),
        template("MATCH p = (a)-[:KNOWS*2..3]->(b) RETURN p")
    );
    // A `*` from arithmetic is the same shape — a false positive that
    // costs a miss, never a wrong key.
    assert_ne!(template("RETURN 2 * 3"), template("RETURN 2 * 4"));
}

/// A function call's first string argument stays literal: it feeds the
/// front-end's constant folding (a temporal constructor's argument folds
/// to a constant at lowering), and a parameter there stays a per-row
/// call — the GBL q8 shape (`date(p.birthday) > date("1990-01-01")`)
/// measured 3.5x slower before this guard.
#[test]
fn function_first_arguments_stay() {
    let t = template("WHERE date(p.birthday) > date(\"1990-01-01\")");
    assert!(
        t.contains("'1990-01-01'"),
        "the constructor argument stays: {t}"
    );
    assert!(!t.contains("$__AUTO_STR"), "no placeholder for it: {t}");
    assert_ne!(
        template("RETURN date(\"1990-01-01\") AS d"),
        template("RETURN date(\"1991-01-01\") AS d")
    );
    // The blunt rule covers every function's first string argument —
    // a miss, never a wrong plan.
    assert!(template("RETURN foo(\"x\") AS y").contains("'x'"));
    // Every argument position and kind: the GBL q8 map form
    // (`datetime({year: 2021})`) and later arguments stay literal too —
    // a parameter there loses the constant fold (found in review).
    let dt = template("RETURN datetime({year: 2021}) AS d");
    assert!(
        dt.contains("2021"),
        "the map-form argument stays literal: {dt}"
    );
    assert!(!dt.contains("$__AUTO_INT"), "no placeholder for it: {dt}");
    assert!(template("RETURN left('abc', 2) AS s").contains("'abc'"));
    // Non-argument positions still normalize.
    assert_eq!(
        template("WHERE n.name = 'alice'"),
        "WHERE n . name = $__AUTO_STR_0__"
    );
    // A bracketed list outside any call still collapses.
    assert_eq!(template("RETURN [1, 2]"), "RETURN $__AUTO_LIST_0__");
    // A list AS a call argument stays literal too — the collapse arm
    // gates on the call-argument context exactly like the scalar arms
    // (found in review): the whole-group rewrite must not bypass the
    // guard the element arms apply.
    let list_arg = template("RETURN f([1, 2]) AS x");
    assert!(
        list_arg.contains("[ 1 , 2 ]"),
        "the list argument stays literal, element-wise: {list_arg}"
    );
    assert!(
        !list_arg.contains("$__AUTO_LIST"),
        "no whole-group placeholder inside a call: {list_arg}"
    );
}

/// Parameters render with their `$`, so a parameter and a same-named
/// variable never share a template; param names are structural.
#[test]
fn parameters_render_with_dollar() {
    assert_eq!(
        template("MATCH (n {id: $uid}) RETURN n"),
        "MATCH ( n { id : $uid } ) RETURN n"
    );
    assert_ne!(
        template("MATCH (n {id: $uid}) RETURN n"),
        template("MATCH (n {id: uid}) RETURN n")
    );
    assert_ne!(
        template("MATCH (n {id: $uid}) RETURN n"),
        template("MATCH (n {id: $other}) RETURN n")
    );
}

/// Backtick-quoted identifiers render through the canonical quoting
/// policy, so a name that cannot appear bare never aliases a bare one.
#[test]
fn quoted_identifiers_render_quoted() {
    let t = template("MATCH (`a b`) RETURN `a b`");
    assert!(t.contains("`a b`"), "the quoted name stays quoted: {t}");
    assert_ne!(t, template("MATCH (a) RETURN a"));
    assert_ne!(template("RETURN `MATCH`"), template("RETURN MATCH"));
}

/// An interpolated string bails the whole statement: its payload embeds
/// expressions — including parameter references the token-level
/// reserved-name check cannot see (one token holds the whole body), so an
/// embedded `$__AUTO_*__` could collide with the rewritten text's own
/// placeholders. Found in review.
#[test]
fn interpolated_strings_bail_the_statement() {
    assert!(matches!(
        norm("RETURN s'a{x}' AS s"),
        NormalizeOutcome::NotCacheable
    ));
}

/// The obfuscated literal is a single dummy value: it renders as written,
/// and every spelling of it shares one template (the same parse).
#[test]
fn obfuscated_literal_renders_as_written() {
    let gated = ParseOptions {
        obfuscated_literals: true,
        ..ParseOptions::default()
    };
    let t = match normalize_statement("RETURN ******", gated) {
        NormalizeOutcome::Normalized(n) => n.template,
        NormalizeOutcome::NotCacheable => panic!("expected the gated form to normalize"),
    };
    assert_eq!(t, "RETURN ******");
}

/// A statement referencing an `$__AUTO_*` parameter bails: the caller can
/// never collide with the engine's own placeholder names. The check is
/// exact-case — parameter names are case-sensitive, and every generated
/// name is the uppercase prefix, so a lowercase `__auto_` spelling cannot
/// collide and stays cacheable.
#[test]
fn auto_param_prefix_in_statement_bails() {
    assert!(matches!(
        norm("RETURN $__AUTO_INT_0__"),
        NormalizeOutcome::NotCacheable
    ));
    assert!(matches!(
        norm("RETURN $__AUTO_LIST_9__"),
        NormalizeOutcome::NotCacheable
    ));
    assert!(matches!(
        norm("RETURN $__auto_x__"),
        NormalizeOutcome::Normalized(_)
    ));
}

/// A lexer error token (the statement will not parse) and oversize input
/// bail — nothing to cache, never an error.
#[test]
fn error_tokens_and_oversize_bail() {
    assert!(matches!(
        norm("RETURN 1B2c3e67"),
        NormalizeOutcome::NotCacheable
    ));
    let oversized = format!("RETURN {}", "1".repeat(MAX_QUERY_BYTES));
    assert!(matches!(norm(&oversized), NormalizeOutcome::NotCacheable));
}

/// The rewritten statement parses whenever the original does — the
/// fail-closed cases (`CYPHER <version>` and any other position whose
/// parameter form is ungrammatical) are the ones whose rewritten parse
/// fails, so those statements simply stay uncached.
#[test]
fn rewritten_statement_parses() {
    for src in [
        "MATCH (n:User {id: 42}) RETURN n",
        "MATCH (n:User {id: 'alice'}) RETURN n.name",
        "MATCH (n) WHERE n.age > 21 AND n.name STARTS WITH 'a' RETURN n ORDER BY n.age LIMIT 5",
        "MATCH (n) WHERE n.t IN [1, 2, 3] RETURN n",
        "RETURN { 'name': 'x', age: 5 } AS m",
        "UNWIND [1, 2] AS x RETURN x",
        "MATCH (n {id: $uid}) RETURN n",
        "match (n:person) return n limit 10",
    ] {
        crate::parser::parse(src).expect("the original parses");
        let n = normalized(src);
        crate::parser::parse(&n.rewritten)
            .unwrap_or_else(|e| panic!("rewritten of {src:?} parses: {e}\n{}", n.rewritten));
    }
}

/// The documented fail-closed case: the `CYPHER <version>` integer takes
/// a literal only, so its rewritten text does not parse and the statement
/// stays uncached.
#[test]
fn cypher_preamble_integer_fails_closed() {
    let n = normalized("CYPHER 25 MATCH (n) RETURN n");
    assert!(
        n.rewritten.contains("$__AUTO_INT_0__"),
        "the version became a placeholder: {}",
        n.rewritten
    );
    assert!(crate::parser::parse(&n.rewritten).is_err());
}

/// Whitespace and comments never affect the template. A placeholder keeps
/// a separator only when the next source character would glue to it
/// (`1 LIMIT` stays two tokens). An operator stays glued (`1+2`), so a
/// projection's source text round-trips.
#[test]
fn whitespace_and_comments_do_not_affect_the_template() {
    assert_eq!(
        template("MATCH (n:User {id: 42}) RETURN n"),
        template("MATCH\n ( n : User { id: 42 } )\n// find\n RETURN n")
    );
    let n = normalized("MATCH (n:User {id: 42}) RETURN n LIMIT 3");
    crate::parser::parse(&n.rewritten)
        .unwrap_or_else(|e| panic!("the spliced form parses: {e}\n{}", n.rewritten));
    let limited = normalized("RETURN 1 LIMIT 2");
    assert!(
        limited.rewritten.contains("$__AUTO_INT_0__ LIMIT"),
        "keyword stays a separate token: {}",
        limited.rewritten
    );
    let added = normalized("RETURN 1+2");
    assert!(
        added.rewritten.contains("$__AUTO_INT_0__+$__AUTO_INT_1__"),
        "operator stays glued: {}",
        added.rewritten
    );
}

/// String escapes resolve before extraction, so differently-spelled
/// equal values share one template.
#[test]
fn escaped_strings_share_by_value() {
    assert_eq!(template("RETURN 'a\\nb'"), template("RETURN \"a\nb\""));
    assert_eq!(
        values("RETURN 'a\\nb'"),
        vec![("__AUTO_STR_0__".into(), NormalizedValue::Str("a\nb".into()))]
    );
}

/// The list scan's recursion is depth-capped at the parser's own
/// ADR-0257 limit: an adversarially nested payload declines (no
/// collapse) instead of recursing past what the parser would reject —
/// the normalizer runs before the parse, so it must not overflow where
/// the parse would have failed cleanly. The parser-side assert runs on
/// the big-stack runner like every other depth test in this crate.
#[test]
fn deeply_nested_lists_decline_instead_of_recursing() {
    // The property the guards exist for is bounded work BEFORE the parse:
    // the depth cap bounds each descent, and the total step budget stops
    // the declined-deep-group re-descend pattern (every run-loop bracket
    // restarts its scan at depth 0) from turning an adversarial payload
    // into quadratic token visits — the budget trips and the whole
    // statement declines to not-cacheable. A 100k-deep balanced payload
    // (well under the 4 MiB text cap) would overflow every thread without
    // the depth cap and burn unbounded steps without the budget.
    let depth = 100_000;
    let deep = format!("RETURN {}{}", "[".repeat(depth), "]".repeat(depth));
    assert_eq!(norm(&deep), NormalizeOutcome::NotCacheable);
    let parsed = relate_test_support::run_with_big_stack(move || crate::parser::parse(&deep));
    assert!(parsed.is_err(), "the parser's depth guard rejects it");
    // A balanced group well inside both bounds still collapses to one
    // param.
    let inside = format!("RETURN {}1{}", "[".repeat(8), "]".repeat(8));
    let NormalizeOutcome::Normalized(inner) = norm(&inside) else {
        panic!("the inside-cap payload normalizes");
    };
    assert_eq!(inner.template, "RETURN $__AUTO_LIST_0__");
}
