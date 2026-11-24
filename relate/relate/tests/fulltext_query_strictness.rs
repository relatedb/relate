//! The fulltext query syntax this engine does not implement is a typed
//! syntax error on both entry points — the SEARCH clause and the
//! `db.index.fulltext.query*` procedures — never a flat-OR reading of the
//! words inside it (REL8-2031 #43, live Neo4j 2026.10.2 answers the same
//! queries). The prose forms live in `docs/reference/cypher-syntax.md`.

mod common;

use common::connection;
use common::fulltext::assert_syntax_error_on_both_paths;
use common::fulltext::fulltext_agree;
use common::fulltext::sorted_names;
use common::run;

/// The issue-43 store: six documents and one fulltext index over title and
/// body, enough data for every rejected construct to have told two readings
/// apart.
fn strictness_store() -> relate::Connection {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (:Doc {id: 1, title: 'alpha beta', body: 'one'}), \
         (:Doc {id: 2, title: 'beta alpha', body: 'two'}), \
         (:Doc {id: 3, title: 'alpha', body: 'beta'}), \
         (:Doc {id: 4, title: 'gamma', body: 'alpha'}), \
         (:Doc {id: 5, title: 'beta gamma', body: 'delta'}), \
         (:Doc {id: 6, title: 'carrot', body: 'one'})",
    );
    run(
        &mut c,
        "CREATE FULLTEXT INDEX ft FOR (n:Doc) ON EACH [n.title, n.body]",
    );
    run(&mut c, "CALL db.awaitIndexes()");
    c
}

/// Grouping, field groups, ranges, regular expressions, the `+`/`-`/`!`
/// modifiers and multi-term phrases: each is rejected, where the parser
/// before the fix answered a plausible-looking wrong set.
#[test]
fn unsupported_lucene_constructs_are_syntax_errors_on_both_paths() {
    let mut c = strictness_store();
    assert_syntax_error_on_both_paths(
        &mut c,
        ("Doc", "ft"),
        &[
            "(gamma OR one) AND beta",
            "beta AND (gamma OR one)",
            "alpha OR (beta AND gamma)",
            "body:(alpha OR delta)",
            "title:(alpha)",
            "title:[b TO c]",
            "title:{alpha TO carrot}",
            "/gam.*/",
            "+alpha +beta",
            "alpha -beta",
            "alpha !beta",
            "alpha AND !beta",
            "alpha AND -beta",
            "\"alpha beta\"",
            "\"alpha beta\"~2",
        ],
    );
}

/// A malformed operator sequence is rejected too: the previous parser read
/// `alpha AND` as `alpha` and `alpha OR OR gamma` as `alpha OR gamma`.
#[test]
fn malformed_operator_queries_are_syntax_errors_on_both_paths() {
    let mut c = strictness_store();
    assert_syntax_error_on_both_paths(
        &mut c,
        ("Doc", "ft"),
        &[
            "alpha AND",
            "alpha OR OR gamma",
            "alpha AND AND beta",
            "AND alpha",
            "OR alpha",
            "",
        ],
    );
}

/// The controls: the supported forms keep answering what live answers,
/// through the SEARCH clause and the procedure alike — the shared helper
/// asserts the two entry points agree.
#[test]
fn the_supported_forms_still_answer() {
    let mut c = strictness_store();
    for (query, expected) in [
        ("alpha AND beta", vec!["alpha", "alpha beta", "beta alpha"]),
        ("alpha AND NOT beta", vec!["gamma"]),
        ("body:alpha OR body:delta", vec!["beta gamma", "gamma"]),
        // A quoted single term is literal text, the documented form.
        (
            "\"alpha\"",
            vec!["alpha", "alpha beta", "beta alpha", "gamma"],
        ),
        // A quoted term after a field prefix is that field's term.
        ("title:\"alpha\"", vec!["alpha", "alpha beta", "beta alpha"]),
    ] {
        assert_eq!(
            sorted_names(&fulltext_agree(&mut c, "Doc", "ft", "title", 10, query)),
            expected,
            "{query}"
        );
    }
}
