//! Normalizer units (ADR-0503): the template key, the guards, the
//! rewritten text, and the not-cacheable bailouts.

use super::*;
use crate::auto_parameter::{authored_spellings, substitute_auto_parameters, AuthoredSpellings};
use NormalizedValue::Int;

/// Normalize with every preview gate closed (the dialect floor).
fn norm(src: &str) -> NormalizeOutcome {
    normalize_statement(src, ParseOptions::default())
}

/// [`normalized`] under explicit options — the one gated-outcome unwrap,
/// so a preview-gated scenario never re-spells the match and its panic.
fn normalized_with(src: &str, options: ParseOptions) -> Normalized {
    match normalize_statement(src, options) {
        NormalizeOutcome::Normalized(n) => n,
        NormalizeOutcome::NotCacheable => panic!("expected {src:?} to normalize"),
    }
}

fn normalized(src: &str) -> Normalized {
    normalized_with(src, ParseOptions::default())
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

/// A postfix bracket — a subscript or slice on the expression before it —
/// keeps its brackets and its interior literals normalize individually:
/// the collapse that serves a list literal (one bare list parameter for
/// the whole group) would glue the placeholder to the expression with no
/// brackets (`labels(n)$__AUTO_LIST_0__`, a syntax error), so every such
/// statement's rewritten compile failed and it was a permanent cache miss
/// (REL8-1423's bracketed-literal item). The rewritten form is the shape
/// an explicit parameter subscript already parses as.
#[test]
fn subscripts_keep_their_brackets() {
    // Function result subscripted — the sqlite-gate PR mix's `kinds` shape.
    let n = normalized("MATCH (n) RETURN labels(n)[0] AS kind");
    assert_eq!(
        n.template,
        "MATCH ( n ) RETURN labels ( n ) [ $__AUTO_INT_0__ ] AS kind"
    );
    assert_eq!(
        n.rewritten,
        "MATCH (n) RETURN labels(n)[$__AUTO_INT_0__] AS kind"
    );
    assert_eq!(n.values, vec![("__AUTO_INT_0__".into(), Int(0))]);
    assert_eq!(n.spellings, vec!["0".to_string()]);

    // A variable, a property, and a list literal subscripted — all postfix.
    assert_eq!(template("RETURN xs[0]"), "RETURN xs [ $__AUTO_INT_0__ ]");
    assert_eq!(
        template("MATCH (n) RETURN n.tags[1] AS t"),
        "MATCH ( n ) RETURN n . tags [ $__AUTO_INT_0__ ] AS t"
    );
    assert_eq!(
        template("RETURN [1, 2, 3][0] AS first"),
        "RETURN $__AUTO_LIST_0__ [ $__AUTO_INT_1__ ] AS first"
    );
    // The list literal itself still collapses; only the subscript opens.
    assert_eq!(
        values("RETURN [1, 2, 3][0] AS first"),
        vec![
            (
                "__AUTO_LIST_0__".into(),
                NormalizedValue::List(vec![Int(1), Int(2), Int(3)])
            ),
            ("__AUTO_INT_1__".into(), Int(0)),
        ]
    );

    // A slice keeps the bounds' own guards: the `..`-following bound stays
    // (the quantifier-bound guard), the opening bound normalizes.
    assert_eq!(
        template("RETURN xs[1..3] AS s"),
        "RETURN xs [ $__AUTO_INT_0__ .. 3 ] AS s"
    );

    // A map literal subscripted by a string key: postfix after `}`, the
    // lookup key normalizes.
    assert_eq!(
        template("RETURN {'a': 1}['a'] AS v"),
        "RETURN { 'a' : $__AUTO_INT_0__ } [ $__AUTO_STR_1__ ] AS v"
    );

    // A nested list whose element is a subscripted list: the inner list
    // literal collapses (prefix, inside the outer group's fallback walk),
    // the subscript keeps its brackets.
    assert_eq!(
        template("RETURN [[1, 2][0], 3] AS xs"),
        "RETURN [ $__AUTO_LIST_0__ [ $__AUTO_INT_1__ ] , $__AUTO_INT_2__ ] AS xs"
    );
}

/// The keyword operands: the literal atoms — TRUE/FALSE/NULL/INFINITY/
/// NaN, one vocabulary at [`is_operand_atom`] — a CASE's END, and the
/// gated obfuscated literal always end an expression, and a keyword in
/// property-name position (`n.order`, `n.end` — the dialect keeps keyword
/// spellings as names, ADR-0169) is an operand because the `.` before it
/// says so.
#[test]
fn keyword_operands_keep_their_brackets() {
    assert_eq!(
        template("RETURN true[0]"),
        "RETURN true [ $__AUTO_INT_0__ ]"
    );
    assert_eq!(
        template("RETURN Infinity[0]"),
        "RETURN Infinity [ $__AUTO_INT_0__ ]"
    );
    assert_eq!(template("RETURN NaN[0]"), "RETURN NaN [ $__AUTO_INT_0__ ]");
    assert_eq!(
        template("RETURN CASE WHEN true THEN [1] ELSE [2] END[0] AS x"),
        "RETURN CASE WHEN true THEN $__AUTO_LIST_0__ ELSE $__AUTO_LIST_1__ END [ $__AUTO_INT_2__ ] AS x"
    );
    assert_eq!(
        template("MATCH (n) RETURN n.order[0] AS o"),
        "MATCH ( n ) RETURN n . order [ $__AUTO_INT_0__ ] AS o"
    );
    assert_eq!(
        template("RETURN n.end[0] AS e"),
        "RETURN n . end [ $__AUTO_INT_0__ ] AS e"
    );
    // The gated obfuscated literal: an operand like TRUE, so its bracket
    // is a subscript.
    assert_eq!(
        normalized_with(
            "RETURN ******[0]",
            ParseOptions {
                obfuscated_literals: true,
                ..ParseOptions::default()
            }
        )
        .template,
        "RETURN ****** [ $__AUTO_INT_0__ ]"
    );
}

/// Keyword-spelled VARIABLES: a keyword after an operand-INTRODUCING
/// token — an operator, an opener, a separator, or an expression-starting
/// keyword, the one vocabulary at [`introduces_operand`] — is in operand
/// position, so the `[` after it is a subscript. One case per vocabulary
/// group, so an omitted entry is a red test: `RETURN` (the
/// expression-starting keywords), `+` and `AND` (the operators), `<` (the
/// comparison family), `[` (the openers). The same keywords in
/// clause/operator position still introduce list literals (the
/// complements in `list_literals_still_collapse_in_clause_position`).
#[test]
fn keyword_variables_in_operand_position_keep_their_brackets() {
    // After an expression-starting keyword.
    assert_eq!(
        template("WITH [1, 2] AS count RETURN count[0] AS c"),
        "WITH $__AUTO_LIST_0__ AS count RETURN count [ $__AUTO_INT_1__ ] AS c"
    );
    // The doubly-ambiguous spelling: the inner `RETURN` is the variable
    // (ADR-0169), the outer one the clause.
    assert_eq!(
        template("RETURN RETURN[0] AS r"),
        "RETURN RETURN [ $__AUTO_INT_0__ ] AS r"
    );
    // After an arithmetic operator.
    assert_eq!(
        template("RETURN 1 + count[0] AS c"),
        "RETURN $__AUTO_INT_0__ + count [ $__AUTO_INT_1__ ] AS c"
    );
    // After a boolean operator.
    assert_eq!(
        template("MATCH (n) WHERE n.x > 1 AND count[0] < 2 RETURN n"),
        "MATCH ( n ) WHERE n . x > $__AUTO_INT_0__ AND count [ $__AUTO_INT_1__ ] < $__AUTO_INT_2__ RETURN n"
    );
    // After a comparison operator.
    assert_eq!(
        template("RETURN 1 < count[0] AS c"),
        "RETURN $__AUTO_INT_0__ < count [ $__AUTO_INT_1__ ] AS c"
    );
    // After a grouping opener.
    assert_eq!(
        template("RETURN [count[0], 2] AS xs"),
        "RETURN [ count [ $__AUTO_INT_0__ ] , $__AUTO_INT_1__ ] AS xs"
    );
}

/// The same keyword token in CLAUSE position still starts a list literal —
/// the `.` before the keyword is the disambiguator, and this is the
/// collapse the list-literal rule exists for.
#[test]
fn list_literals_still_collapse_in_clause_position() {
    assert_eq!(template("RETURN [1, 2]"), "RETURN $__AUTO_LIST_0__");
    assert_eq!(
        template("MATCH (n) WHERE n.x IN [1, 2] RETURN n"),
        "MATCH ( n ) WHERE n . x IN $__AUTO_LIST_0__ RETURN n"
    );
}

/// Brackets after a `-` render as written: the token is a relationship
/// pattern head or binary subtraction, and the classification cannot tell
/// them apart — both keep their brackets. The pattern side: the non-empty
/// interior never scans as literals, and the ANONYMOUS `[]` scans as an
/// empty — clean — group, so without the exclusion it collapsed and the
/// rewritten `-$__AUTO_LIST_0__->` could not parse (every anonymous-rel
/// statement was a permanent cache miss, literals or not). The
/// subtraction side: the list renders with its brackets and its elements
/// normalize individually — a coarser template, never a broken one.
#[test]
fn brackets_after_a_dash_render_as_written() {
    assert_eq!(
        template("MATCH (a)-[r:KNOWS]->(b) RETURN b"),
        "MATCH ( a ) - [ r : KNOWS ] -> ( b ) RETURN b"
    );
    assert_eq!(
        template("MATCH (a)-[]->(b) RETURN b"),
        "MATCH ( a ) - [ ] -> ( b ) RETURN b"
    );
    assert_eq!(
        template("MATCH (a)<-[]-(b) RETURN b"),
        "MATCH ( a ) <- [ ] - ( b ) RETURN b"
    );
    // The subtraction half of the ambiguity.
    assert_eq!(
        template("RETURN 1 - [1, 2] AS d"),
        "RETURN $__AUTO_INT_0__ - [ $__AUTO_INT_1__ , $__AUTO_INT_2__ ] AS d"
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

/// LABS-1602: a parameter name that is not identifier-shaped — a space,
/// an embedded backtick — renders through the canonical backtick-quoted
/// form, so the template re-parses to the same one parameter and two
/// different names never share a template (a raw `$<text>` render let a
/// spaced name collide with the bare prefix's own text).
#[test]
fn parameter_names_render_through_the_canonical_shaped_form() {
    assert_eq!(
        template("MATCH (n {id: $`p 1`}) RETURN n"),
        "MATCH ( n { id : $`p 1` } ) RETURN n"
    );
    assert_eq!(
        template("MATCH (n {id: $`a``b`}) RETURN n"),
        "MATCH ( n { id : $`a``b` } ) RETURN n"
    );
    // Distinct names stay distinct templates.
    assert_ne!(
        template("MATCH (n {id: $`p 1`}) RETURN n"),
        template("MATCH (n {id: $p1}) RETURN n")
    );
    // The two spellings of one name share one template — the shaped form
    // is the name's canonical spelling, so `$`p1`` hits the plan-cache
    // entry `$p1` already warmed.
    assert_eq!(
        template("MATCH (n {id: $`p1`}) RETURN n"),
        template("MATCH (n {id: $p1}) RETURN n")
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
/// and every spelling of it shares one template (the same parse). Its
/// bracket-role classification (an operand, like TRUE) is pinned in
/// `keyword_operands_keep_their_brackets`.
#[test]
fn obfuscated_literal_renders_as_written() {
    let gated = ParseOptions {
        obfuscated_literals: true,
        ..ParseOptions::default()
    };
    assert_eq!(
        normalized_with("RETURN ******", gated).template,
        "RETURN ******"
    );
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
        // Subscript positions (REL8-1423): the rewritten form must keep
        // the brackets — the collapse's bare-placeholder form was the
        // permanent-miss defect this round-trip property pins.
        "MATCH (n) RETURN labels(n)[0] AS kind",
        "RETURN [1, 2, 3][0] AS first",
        "MATCH (n) RETURN n.tags[1] AS t",
        "RETURN xs[1..3] AS s",
        "RETURN {'a': 1}['a'] AS v",
        "RETURN [[1, 2][0], 3] AS xs",
        // The keyword-operand positions the preceding-token-only form of
        // the rule missed (stellar round 1): atoms, a CASE's END, and
        // keyword-spelled property names.
        "RETURN true[0]",
        "RETURN Infinity[0]",
        "RETURN NaN[0]",
        "RETURN CASE WHEN true THEN [1] ELSE [2] END[0] AS x",
        "MATCH (n) RETURN n.order[0] AS o",
        "RETURN n.end[0] AS e",
        // Keyword-spelled variables in operand position (stellar round 4).
        "WITH [1, 2] AS count RETURN count[0] AS c",
        "MATCH (n) WITH collect(n) AS nodes RETURN nodes[0] AS n0",
        "RETURN RETURN[0] AS r",
        // The anonymous relationship pattern: its empty `[]` must not
        // collapse (stellar round 2) — a statement may carry both.
        "MATCH (a)-[]->(b) WHERE a.x = 1 RETURN b",
        // A subtraction's list: the `-` is indistinguishable from a
        // pattern head, so the bracket keeps its brackets.
        "RETURN 1 - [1, 2] AS d",
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

/// The authored-spelling substitution (ADR-0718): a placeholder in
/// rendered column text becomes its authored spelling, matched by exact
/// name, quote-aware, in a single pass.
mod substitute {
    use super::*;

    /// The spellings of a normalization, zipped through the one
    /// construction — the substitution's own fixture.
    fn spellings_of(src: &str) -> AuthoredSpellings {
        let NormalizeOutcome::Normalized(normalized) = norm(src) else {
            panic!("expected {src:?} to normalize");
        };
        authored_spellings(&normalized.values, &normalized.spellings)
    }

    /// Every extracted literal's placeholder substitutes back to its
    /// authored source spelling — the plain column shapes.
    #[test]
    fn a_placeholder_substitutes_to_its_authored_spelling() {
        assert_eq!(
            substitute_auto_parameters("$__AUTO_INT_0__", &spellings_of("RETURN 1")),
            "1"
        );
        assert_eq!(
            substitute_auto_parameters(
                "$__AUTO_INT_0__ + $__AUTO_INT_1__",
                &spellings_of("RETURN 1 + 2")
            ),
            "1 + 2"
        );
        // A string literal's spelling keeps its quotes; a list's keeps its
        // brackets — the authored name is the source text.
        assert_eq!(
            substitute_auto_parameters("$__AUTO_STR_0__", &spellings_of("RETURN 'x'")),
            "'x'"
        );
        assert_eq!(
            substitute_auto_parameters("$__AUTO_LIST_0__", &spellings_of("RETURN [1, 2]")),
            "[1, 2]"
        );
        // Nothing carried: the text passes through untouched.
        assert_eq!(
            substitute_auto_parameters(
                "n.x + $myParam",
                &spellings_of("MATCH (n) RETURN n.x + $myParam")
            ),
            "n.x + $myParam"
        );
    }

    /// A lookalike that is not a carried name stays verbatim — the
    /// call-argument guard keeps string literals out of extraction, so a
    /// placeholder-shaped string's content must survive the substitution
    /// of a sibling extracted literal.
    #[test]
    fn a_lookalike_that_is_not_carried_stays_verbatim() {
        // The call-argument guard keeps the string out of extraction, so
        // the only carried name is the extracted `1`'s `__AUTO_INT_0__`;
        // the string's `$__AUTO_INT_9__` content is not it — verbatim,
        // while the real placeholder substitutes.
        let spellings = spellings_of("RETURN startsWith(n.x, '$__AUTO_INT_9__') + 1");
        assert_eq!(
            spellings
                .iter()
                .map(|spelling| spelling.placeholder.as_str())
                .collect::<Vec<_>>(),
            vec!["__AUTO_INT_0__"]
        );
        assert_eq!(
            substitute_auto_parameters(
                "startsWith(n.x, '$__AUTO_INT_9__') + $__AUTO_INT_0__",
                &spellings
            ),
            "startsWith(n.x, '$__AUTO_INT_9__') + 1"
        );
    }

    /// A carried name inside a string literal is never substituted from
    /// within its quotes — the quote-aware scan refuses the one case an
    /// exact-name match alone cannot.
    #[test]
    fn a_carried_name_inside_a_string_literal_stays_verbatim() {
        // The extracted `1` carries `__AUTO_INT_0__`; the string's content
        // spells the SAME name. Only the unquoted occurrence substitutes.
        let spellings = spellings_of("RETURN startsWith(n.x, '$__AUTO_INT_0__') + 1");
        assert_eq!(
            substitute_auto_parameters(
                "startsWith(n.x, '$__AUTO_INT_0__') + $__AUTO_INT_0__",
                &spellings
            ),
            "startsWith(n.x, '$__AUTO_INT_0__') + 1"
        );
        // A backslash-escaped quote closes nothing; the doubled-quote-free
        // grammar keeps the span open until the real closer.
        // The string is a call argument (not extracted), so the only
        // carried name is the extracted `1`'s `__AUTO_INT_0__`; the
        // backslash-escaped quote keeps the string span open through the
        // lookalike, and only the unquoted occurrence substitutes.
        let spellings = spellings_of(r#"RETURN startsWith(n.x, 'a\'b') + 1"#);
        assert_eq!(
            spellings[0].placeholder, "__AUTO_INT_0__",
            "the call-argument string is not extracted; only the 1 is"
        );
        assert_eq!(
            substitute_auto_parameters(
                r#"startsWith(n.x, 'a\'$__AUTO_INT_0__') + $__AUTO_INT_0__"#,
                &spellings
            ),
            r#"startsWith(n.x, 'a\'$__AUTO_INT_0__') + 1"#
        );
    }

    /// The output is never re-scanned: an extracted string whose authored
    /// spelling itself contains placeholder-lookalike text lands once.
    #[test]
    fn a_spelling_containing_lookalike_text_lands_once() {
        let spellings = spellings_of("RETURN '$__AUTO_INT_0__'");
        assert_eq!(spellings[0].placeholder, "__AUTO_STR_0__");
        assert_eq!(
            substitute_auto_parameters("$__AUTO_STR_0__", &spellings),
            "'$__AUTO_INT_0__'"
        );
    }

    /// The splice preserves the literal span's trailing whitespace, so a
    /// multi-space original's spacing survives into the plan's column
    /// text and the substitution reproduces the source exactly.
    #[test]
    fn multi_space_spacing_survives_the_substituted_name() {
        let spellings = spellings_of("RETURN 1  +  2");
        assert_eq!(
            substitute_auto_parameters("$__AUTO_INT_0__  +  $__AUTO_INT_1__", &spellings),
            "1  +  2"
        );
    }

    /// A truncated or malformed name-shaped run copies verbatim: the
    /// reader refuses a missing index, a missing closing `__`, or a bare
    /// prefix — the `$` falls through as ordinary text.
    #[test]
    fn malformed_name_shapes_copy_verbatim() {
        let spellings = spellings_of("RETURN 1");
        for text in [
            "$__AUTO_INT_",
            "$__AUTO_INT_x__",
            "$__AUTO_",
            "$__AUTO_INT_0_",
            "$myParam",
            "$",
        ] {
            assert_eq!(
                substitute_auto_parameters(text, &spellings),
                text,
                "{text:?} copies verbatim"
            );
        }
    }
}
