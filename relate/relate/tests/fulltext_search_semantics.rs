//! CIP-225 fulltext SEARCH semantics and the `db.index.fulltext.query*`
//! procedures, pinned against live Neo4j 2026.10.1 (enterprise, scratch
//! database): the Lucene query syntax passes through (`AND`/`NOT`, a
//! lowercase `and` staying a term), the hit order is live's BM25 ranking
//! (the calibrated score values are asserted directly), `SKIP`/`OFFSET`
//! page that ranked list, `WITH ANALYZER` is a per-row expression, and the
//! procedures run the same scanner (multi-term OR over every indexed
//! property). The docs family pins the analyzer-side semantics of
//! ADR-0636 — an index's configured analyzer analyzes its stored tokens
//! and its no-override queries (the reported repro: `refunds` finds
//! `refund` on an `english` index) — rather than live probes.

mod common;

use common::connection;
use common::first_column;
use common::first_column_strings as titles;
use common::fulltext::assert_syntax_error_on_both_paths;
use common::fulltext::fulltext_agree;
use common::fulltext::fulltext_procedure;
use common::fulltext::fulltext_search;
use common::fulltext::scored;
use common::fulltext::scored_names;
use common::fulltext::sorted_names;
use common::fulltext::AnalyzerOverride;
use common::run;
use relate::Value;

/// The FulltextSearchAcceptance background: the three fulltext indexes and
/// the six movie plots, with the index columns the scenarios search.
fn fulltext_store() -> relate::Connection {
    let mut c = connection();
    for statement in [
        "CREATE FULLTEXT INDEX moviePlots IF NOT EXISTS FOR (m:Movie) ON EACH [m.plot]",
        "CREATE FULLTEXT INDEX mediaPlots IF NOT EXISTS FOR (m:Movie | Book) ON EACH [m.plot]",
        "CREATE FULLTEXT INDEX movieTitlesAndPlots IF NOT EXISTS \
         FOR (m:Movie) ON EACH [m.title, m.plot]",
    ] {
        run(&mut c, statement);
    }
    run(&mut c, "CALL db.awaitIndexes()");
    run(
        &mut c,
        "CREATE (aladdin:Movie {title: 'Aladdin', plot: 'A street urchin finds a magic lamp \
           holding a wish-granting genie.', rating: 8.0}), \
         (frozen:Movie {title: 'Frozen', plot: 'A fearless princess journeys to find her \
           estranged sister the snow queen and save their kingdom from eternal winter.', \
           rating: 7.4}), \
         (mulan:Movie {title: 'Mulan', plot: 'A young woman disguises herself as a soldier to \
           take the place of her father in the army.', rating: 7.7}), \
         (snow:Movie {title: 'Snow White', plot: 'A gentle princess flees a jealous queen and \
           shelters with seven dwarfs.', rating: 7.6}), \
         (lion:Movie {title: 'Lion King', plot: 'A lion cub prince flees his kingdom after the \
           death of his father the king.', rating: 8.5}), \
         (pippi:Book {title: 'Pippi', plot: 'A strong red-haired girl lives alone with a horse \
           and a monkey.', rating: 7.3})",
    );
    c
}

/// FulltextSearchAcceptance [55]: the Lucene operators reach the index
/// verbatim — `princess AND NOT snow` is Snow White (Frozen has both
/// tokens, Snow White has no `snow` in its plot).
#[test]
fn query_syntax_passes_through() {
    let mut c = fulltext_store();
    let result = run(
        &mut c,
        "MATCH (m:Movie) \
             SEARCH m IN (FULLTEXT INDEX moviePlots FOR 'princess AND NOT snow' LIMIT 5) \
             RETURN m.title AS title",
    );
    assert_eq!(titles(&result), ["Snow White"]);

    // An explicit OR and juxtaposed terms (implicit OR) are the same
    // two-term query; the multi-property index matches title or plot.
    let result = run(
        &mut c,
        "MATCH (m:Movie) \
             SEARCH m IN (FULLTEXT INDEX movieTitlesAndPlots FOR 'Aladdin OR winter' LIMIT 5) \
             RETURN m.title AS title ORDER BY title ASC",
    );
    assert_eq!(titles(&result), ["Aladdin", "Frozen"]);

    let result = run(
        &mut c,
        "MATCH (m:Movie) \
             SEARCH m IN (FULLTEXT INDEX moviePlots FOR 'princess queen' LIMIT 5) \
             RETURN m.title AS title ORDER BY title ASC",
    );
    assert_eq!(titles(&result), ["Frozen", "Snow White"]);
}

/// FulltextSearchAcceptance [40]: on a multi-label index without a label
/// restriction the boolean query narrows to the index's own entities —
/// only lowercase `and` is a term, `AND`/`NOT` are operators.
#[test]
fn multi_label_index_without_label_restriction() {
    let mut c = fulltext_store();
    let result = run(
        &mut c,
        "MATCH (m) \
             SEARCH m IN ( \
               FULLTEXT INDEX mediaPlots FOR 'princess AND and NOT kingdom' LIMIT 5 \
             ) \
             RETURN m.title AS title ORDER BY title ASC",
    );
    assert_eq!(titles(&result), ["Snow White"]);
}

/// FulltextSearchAcceptance [22]/[23]/[56]: `SKIP`/`OFFSET` page the
/// ranked hits, and the ranking is live's BM25 order — Snow White outranks
/// Frozen on `princess queen` (shorter plot), while `princess queen
/// kingdom` puts three-term Frozen first and one-term Lion King third.
#[test]
fn ranking_matches_live_and_pages_with_skip() {
    let mut c = fulltext_store();
    // The live 2026.10.1 scores for this corpus, asserted to 1e-5 (live
    // rounds through f32; the engine's f64 differs in the last digits).
    let result = run(
        &mut c,
        "MATCH (m:Movie) \
             SEARCH m IN (FULLTEXT INDEX moviePlots FOR 'princess queen' LIMIT 5) \
             SCORE AS score RETURN m.title AS title, score ORDER BY score DESC",
    );
    let hits = scored(&result);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].0, "Snow White");
    assert!((hits[0].1 - 0.8708850741386414).abs() < 1e-5, "{hits:?}");
    assert_eq!(hits[1].0, "Frozen");
    assert!((hits[1].1 - 0.7220360040664673).abs() < 1e-5, "{hits:?}");

    for window in ["SKIP 1", "OFFSET 1"] {
        let result = run(
            &mut c,
            &format!(
                "MATCH (m:Movie) \
                 SEARCH m IN (FULLTEXT INDEX moviePlots FOR 'princess queen' {window} LIMIT 2) \
                 RETURN m.title AS title"
            ),
        );
        assert_eq!(titles(&result), ["Frozen"], "{window}");
    }

    let result = run(
        &mut c,
        "MATCH (m:Movie) \
             SEARCH m IN ( \
               FULLTEXT INDEX moviePlots FOR 'princess queen kingdom' SKIP 1 LIMIT 2 \
             ) \
             RETURN m.title AS title",
    );
    assert_eq!(titles(&result), ["Snow White", "Lion King"]);
}

/// FulltextSearchAcceptance [19]: the query string is a per-row property
/// expression; the ranked top three are live's (Snow White, Frozen, Lion
/// King — the rare shared terms outrank the plot's stop-word-like ones).
#[test]
fn property_query_ranks_like_live() {
    let mut c = fulltext_store();
    let result = run(
        &mut c,
        "MATCH (seed:Movie {title: 'Snow White'}) \
             MATCH (m:Movie) \
             SEARCH m IN (FULLTEXT INDEX moviePlots FOR seed.plot LIMIT 3) \
             RETURN m.title AS title",
    );
    assert_eq!(titles(&result), ["Snow White", "Frozen", "Lion King"]);
}

/// FulltextSearchAcceptance [57]: `WITH ANALYZER` is evaluated per row.
/// The index holds the unstemmed token `princess`, so only the stemming
/// (and stop-word-filtering) `english` analyzer matches `princesses`.
#[test]
fn per_row_analyzer_stems_english() {
    let mut c = fulltext_store();
    let result = run(
        &mut c,
        "UNWIND ['english', 'standard'] AS analyzerName \
             MATCH (m:Movie) \
             SEARCH m IN ( \
               FULLTEXT INDEX moviePlots FOR 'princesses' WITH ANALYZER analyzerName LIMIT 5 \
             ) \
             RETURN analyzerName, m.title AS title ORDER BY analyzerName ASC, title ASC",
    );
    assert_eq!(
        result.rows,
        vec![
            vec![
                Value::String("english".into()),
                Value::String("Frozen".into())
            ],
            vec![
                Value::String("english".into()),
                Value::String("Snow White".into())
            ],
        ]
    );
}

/// The docs store: the same two texts under an `english`-analyzer index
/// (`Doc`) and the default one (`Plain`) — the reported repro's corpus.
fn docs_store() -> relate::Connection {
    let mut c = connection();
    run(
        &mut c,
        "CREATE FULLTEXT INDEX enDocs IF NOT EXISTS FOR (d:Doc) ON EACH [d.text] \
         OPTIONS {indexConfig: {`fulltext.analyzer`: 'english'}}",
    );
    run(
        &mut c,
        "CREATE FULLTEXT INDEX plainDocs IF NOT EXISTS FOR (d:Plain) ON EACH [d.text]",
    );
    run(&mut c, "CALL db.awaitIndexes()");
    run(
        &mut c,
        "CREATE (:Doc {text: 'a refund was issued'}), \
         (:Doc {text: 'the card was charged twice'}), \
         (:Plain {text: 'a refund was issued'}), \
         (:Plain {text: 'the card was charged twice'})",
    );
    c
}

/// The docs store's two sides: the `english`-analyzer index and the
/// default one. Each side's correlated label and index is held once, so a
/// crossed pair cannot be spelled.
#[derive(Clone, Copy)]
enum DocsSide {
    /// `(Doc)` under `enDocs` — the `english` analyzer.
    English,
    /// `(Plain)` under `plainDocs` — the default analyzer.
    Plain,
}

impl DocsSide {
    /// The side's correlated label and fulltext index.
    fn label_and_index(self) -> (&'static str, &'static str) {
        match self {
            Self::English => ("Doc", "enDocs"),
            Self::Plain => ("Plain", "plainDocs"),
        }
    }
}

/// Both entry points over one side of the docs store — the shared
/// agreement helper through the side's correlated pair: the ranked hits.
fn docs_hits(c: &mut relate::Connection, side: DocsSide, query: &str) -> Vec<(String, f64)> {
    let (label, index) = side.label_and_index();
    fulltext_agree(c, label, index, "text", 5, query)
}

/// The ranked names of the docs store's agreement hits.
fn docs_names(c: &mut relate::Connection, side: DocsSide, query: &str) -> Vec<String> {
    scored_names(&docs_hits(c, side, query))
}

/// An `english` index analyzes its stored tokens and the no-override query
/// (ADR-0636): an inflected query term crosses to the stem through both
/// entry points, and the analyzer's stop words match nothing on either
/// side.
#[test]
fn an_english_index_matches_inflections_on_both_entry_points() {
    let mut c = docs_store();
    for (query, expected) in [
        ("refunds", vec!["a refund was issued"]),
        ("refund", vec!["a refund was issued"]),
        ("charge", vec!["the card was charged twice"]),
        ("issued", vec!["a refund was issued"]),
        ("refun*", vec!["a refund was issued"]),
        ("the", vec![]),
        (
            "refund OR charged",
            vec!["a refund was issued", "the card was charged twice"],
        ),
        (
            "refund charged",
            vec!["a refund was issued", "the card was charged twice"],
        ),
    ] {
        assert_eq!(
            docs_names(&mut c, DocsSide::English, query),
            expected,
            "{query}"
        );
    }
}

/// The hits of an implicit-OR query rank by the calibrated BM25, not a
/// constant.
#[test]
fn an_english_index_ranks_hits_by_bm25() {
    let mut c = docs_store();
    let hits = docs_hits(&mut c, DocsSide::English, "refund charged");
    assert_eq!(
        scored_names(&hits),
        ["a refund was issued", "the card was charged twice"]
    );
    assert!(hits[0].1 > hits[1].1, "{hits:?}");
    assert!(hits.iter().all(|(_, score)| *score != 1.0), "{hits:?}");
}

/// The default analyzer (`standard-no-stop-words`) keeps both sides
/// unstemmed and keeps stop words.
#[test]
fn the_default_index_keeps_both_sides_unstemmed() {
    let mut c = docs_store();
    for (query, expected) in [
        ("refunds", vec![]),
        ("refund", vec!["a refund was issued"]),
        ("the", vec!["the card was charged twice"]),
    ] {
        assert_eq!(
            docs_names(&mut c, DocsSide::Plain, query),
            expected,
            "{query}"
        );
    }
}

/// `WITH ANALYZER` overrides the query side only (ADR-0636): the standard
/// analyzer leaves `refunds` unstemmed against the english index's stems,
/// and the english analyzer stems a query against the default index's
/// unstemmed base forms.
#[test]
fn with_analyzer_overrides_the_query_side_only() {
    let mut c = docs_store();
    for (side, query, analyzer, expected) in [
        (
            DocsSide::English,
            "refunds",
            AnalyzerOverride::Named("standard"),
            &[] as &[&str],
        ),
        (
            DocsSide::English,
            "refund",
            AnalyzerOverride::Named("standard"),
            &["a refund was issued"],
        ),
        (
            DocsSide::Plain,
            "refunds",
            AnalyzerOverride::Named("english"),
            &["a refund was issued"],
        ),
    ] {
        let (label, index) = side.label_and_index();
        let hits = fulltext_search(&mut c, label, index, "text", 5, query, analyzer);
        assert_eq!(scored_names(&hits), expected, "{label} {query}");
    }
    // A NULL `WITH ANALYZER` is no override, so the query follows the
    // index's analyzer and the inflection still crosses.
    let hits = fulltext_search(
        &mut c,
        "Doc",
        "enDocs",
        "text",
        5,
        "refunds",
        AnalyzerOverride::Null,
    );
    assert_eq!(scored_names(&hits), ["a refund was issued"]);
}

/// FulltextProcedureDeprecationAcceptance [5]/[6]/[7]: the procedures run
/// the same query parser and scanner as SEARCH — multi-term queries are
/// OR'd over every indexed property (title and plot), and a multi-label /
/// multi-type index scans exactly its own entities.
#[test]
fn fulltext_procedures_share_the_query_syntax() {
    let mut c = connection();
    for statement in [
        "CREATE FULLTEXT INDEX movieTitlesAndPlots IF NOT EXISTS \
         FOR (m:Movie) ON EACH [m.title, m.plot]",
        "CREATE FULLTEXT INDEX mediaPlots IF NOT EXISTS FOR (m:Movie | Book) ON EACH [m.plot]",
        "CREATE FULLTEXT INDEX relNotesMultiType IF NOT EXISTS \
         FOR ()-[r:RELATED_TO | SIMILAR_TO]->() ON EACH [r.note]",
    ] {
        run(&mut c, statement);
    }
    run(&mut c, "CALL db.awaitIndexes()");
    run(
        &mut c,
        "CREATE (:Movie {title: 'Aladdin', plot: 'A street urchin finds a magic lamp.'}) \
           -[:RELATED_TO {note: 'magic lamp'}]-> \
         (:Movie {title: 'Hook', plot: 'A lawyer returns to Neverland.'}) \
           -[:SIMILAR_TO {note: 'lamp of wishes'}]-> \
         (:Book {title: 'Arabian Nights', plot: 'Tales of a magic lamp and a genie.'}), \
         (:Movie {title: 'Lamplighter', plot: 'A quiet night in the city.'})",
    );

    // Multi-property: `lamp` matches Aladdin's plot, `lamplighter` the
    // title of the same-named movie.
    let result = run(
        &mut c,
        "CALL db.index.fulltext.queryNodes('movieTitlesAndPlots', 'lamp lamplighter') \
             YIELD node RETURN node.title AS title",
    );
    let mut found = titles(&result);
    found.sort();
    assert_eq!(found, ["Aladdin", "Lamplighter"]);

    // Multi-label: both the Movie and the Book.
    let result = run(
        &mut c,
        "CALL db.index.fulltext.queryNodes('mediaPlots', 'lamp') \
         YIELD node RETURN node.title AS title",
    );
    let mut found = titles(&result);
    found.sort();
    assert_eq!(found, ["Aladdin", "Arabian Nights"]);

    // Multi-type relationship index.
    let result = run(
        &mut c,
        "CALL db.index.fulltext.queryRelationships('relNotesMultiType', 'lamp') \
             YIELD relationship RETURN relationship.note AS note",
    );
    let mut found = titles(&result);
    found.sort();
    assert_eq!(found, ["lamp of wishes", "magic lamp"]);
}

/// A people index for the fuzzy / boost scenarios: `Kolleger` is the exact
/// spelling the queries use, `Kollegger` the one-insertion neighbour.
fn people_store() -> relate::Connection {
    let mut c = connection();
    run(
        &mut c,
        "CREATE FULLTEXT INDEX personText IF NOT EXISTS FOR (p:Person) ON EACH [p.name, p.bio]",
    );
    run(&mut c, "CALL db.awaitIndexes()");
    run(
        &mut c,
        "CREATE (:Person {name: 'Kollegger', bio: 'graph consultant'}), \
         (:Person {name: 'Kolleger', bio: 'database consultant'}), \
         (:Person {name: 'Anna', bio: 'lamp maker'})",
    );
    c
}

/// Both entry points over the people store's `personText` index — the
/// shared agreement helper through this store's shape: the ranked hits.
fn people_hits(c: &mut relate::Connection, query: &str) -> Vec<(String, f64)> {
    fulltext_agree(c, "Person", "personText", "name", 10, query)
}

/// `term~N` matches near terms (a transposition or one insertion away),
/// ranks the exact spelling above the neighbour, and `~0` / no suffix are
/// exact — through the SEARCH clause and the procedure alike.
#[test]
fn fuzzy_term_matches_near_spellings() {
    let mut c = people_store();
    let hits = people_hits(&mut c, "kolleger~1");
    assert_eq!(scored_names(&hits), ["Kolleger", "Kollegger"]);
    assert!(hits[0].1 > hits[1].1, "{hits:?}");

    for exact in ["kolleger~0", "kolleger"] {
        assert_eq!(
            scored_names(&people_hits(&mut c, exact)),
            ["Kolleger"],
            "{exact}"
        );
    }
    // A bare `~` is Lucene's default distance 2.
    assert_eq!(people_hits(&mut c, "kollger~").len(), 2);
    // The suffix digit is never a term: `~1` must not require a token `1`.
    assert_eq!(people_hits(&mut c, "kolleger~1 OR zzz").len(), 2);
}

/// The reported case: `kolleger~1 AND <other term>` composes — the fuzzy
/// clause is a MUST any-of group, the other term a plain MUST.
#[test]
fn fuzzy_clause_composes_with_boolean_operators() {
    let mut c = people_store();
    for (query, expected) in [
        ("kolleger~1 AND consultant", vec!["Kolleger", "Kollegger"]),
        ("kolleger~1 AND database", vec!["Kolleger"]),
        ("kolleger~1 AND lamp", vec![]),
        ("consultant AND NOT kolleger~0", vec!["Kollegger"]),
        (
            "name:kolleger~1 AND bio:consultant",
            vec!["Kolleger", "Kollegger"],
        ),
    ] {
        assert_eq!(
            scored_names(&people_hits(&mut c, query)),
            expected,
            "{query}"
        );
    }
}

/// `term^B` scales the clause's score without changing which rows match.
#[test]
fn boost_reorders_without_changing_the_match_set() {
    let mut c = people_store();
    let plain = people_hits(&mut c, "consultant lamp");
    let boosted = people_hits(&mut c, "consultant^10 lamp");
    assert_eq!(sorted_names(&plain), sorted_names(&boosted));
    // `lamp` is rarer than `consultant`, so Anna leads until `consultant`
    // is boosted past it.
    assert_eq!(plain[0].0, "Anna", "{plain:?}");
    assert_ne!(boosted[0].0, "Anna", "{boosted:?}");
    // Zero boost keeps the match, scoring 0.
    let zero = people_hits(&mut c, "lamp^0");
    assert_eq!(zero.len(), 1);
    assert_eq!(zero[0].1, 0.0);
    // Boost composes with fuzzy.
    let both = people_hits(&mut c, "kolleger~1^2");
    let single = people_hits(&mut c, "kolleger~1");
    assert!((both[0].1 - 2.0 * single[0].1).abs() < 1e-9, "{both:?}");
}

/// A malformed suffix is a syntax error on both entry points, not a stray
/// term (`lamp^x` must not silently search for `lamp` and `x`).
#[test]
fn malformed_fuzzy_and_boost_suffixes_are_syntax_errors() {
    let mut c = people_store();
    assert_syntax_error_on_both_paths(
        &mut c,
        ("Person", "personText"),
        &["lamp^x", "lamp~1.5", "lamp^-1", "~1"],
    );
}

/// Pins the live Neo4j 2026.09-enterprise probe values: the closeness
/// weight is `1 - edits / min(len)` (0.875 at one edit, 0.75 at two on an
/// 8-character term), integers above 2 clamp, a `0.x` fraction is a
/// minimum similarity, and `~1x` / a detached `~1` follow Lucene's lexer.
/// Scores are corpus-dependent, so the assertions are ratios and sets.
#[test]
fn fuzzy_matches_the_live_probe_values() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE FULLTEXT INDEX nameText IF NOT EXISTS FOR (p:Person) ON EACH [p.name]",
    );
    run(&mut c, "CALL db.awaitIndexes()");
    run(
        &mut c,
        "CREATE (:Person {name: 'Kolleger'}), (:Person {name: 'Kollegger'}), \
         (:Person {name: 'Colleger'}), (:Person {name: 'Kolegger'}), \
         (:Person {name: 'Kolleggar'}), (:Person {name: 'Anna'})",
    );
    // The probe values are the procedure's ranked pairs — the canonical
    // helper through this store's binding.
    let hits =
        |c: &mut relate::Connection, query: &str| fulltext_procedure(c, "nameText", "name", query);
    let score_of = |hits: &[(String, f64)], name: &str| -> f64 {
        hits.iter().find(|(n, _)| n == name).expect(name).1
    };

    let one = hits(&mut c, "kolleger~1");
    assert_eq!(sorted_names(&one), ["Colleger", "Kolleger", "Kollegger"]);
    assert_eq!(one[0].0, "Kolleger");
    assert!((score_of(&one, "Kollegger") / score_of(&one, "Kolleger") - 0.875).abs() < 1e-9);
    assert!((score_of(&one, "Colleger") / score_of(&one, "Kolleger") - 0.875).abs() < 1e-9);

    let two = hits(&mut c, "kolleger~2");
    assert_eq!(
        sorted_names(&two),
        ["Colleger", "Kolegger", "Kolleger", "Kolleggar", "Kollegger"]
    );
    assert!((score_of(&two, "Kolegger") / score_of(&two, "Kolleger") - 0.75).abs() < 1e-9);
    assert!((score_of(&two, "Kolleggar") / score_of(&two, "Kolleger") - 0.75).abs() < 1e-9);

    // Bare `~` is 2; integers above 2 clamp; `~0` is exact.
    for same_as_two in ["kolleger~", "kolleger~3", "kolleger~99"] {
        assert_eq!(hits(&mut c, same_as_two), two, "{same_as_two}");
    }
    assert_eq!(sorted_names(&hits(&mut c, "kolleger~0")), ["Kolleger"]);
    // `~0.8` is a minimum similarity: floor(0.2 * 8) = one edit.
    assert_eq!(hits(&mut c, "kolleger~0.8"), one);
    // A detached `~1` binds to the previous word.
    assert_eq!(hits(&mut c, "kolleger ~1"), one);
    // `~1x` is a bare `~` plus the separate term `1x`.
    assert_eq!(hits(&mut c, "kolleger~1x"), two);
    // A boost scales the clause exactly.
    let boosted = hits(&mut c, "kolleger~1^2");
    for (name, score) in &one {
        assert!(
            (score_of(&boosted, name) - 2.0 * score).abs() < 1e-9,
            "{name}"
        );
    }
    // `^1.5` scales by 1.5.
    let plain = hits(&mut c, "anna");
    let scaled = hits(&mut c, "anna^1.5");
    assert!((scaled[0].1 - 1.5 * plain[0].1).abs() < 1e-9);
}

/// The small store the `options`-map test ranks: four documents whose
/// `apple` term frequencies order them 1, 3, 2, and two relationships.
fn options_store() -> relate::Connection {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (:Opt {id: 1, t: 'apple apple apple'}), (:Opt {id: 2, t: 'apple pear plum'}), \
         (:Opt {id: 3, t: 'apple'}), (:Opt {id: 4, t: 'pear'}), (:Opt {id: 5, t: 'the lamp'})",
    );
    run(
        &mut c,
        "CREATE (a:Rel {id: 1}), (b:Rel {id: 2}), \
         (a)-[:R {note: 'apple'}]->(b), (b)-[:R {note: 'apple road'}]->(a)",
    );
    run(
        &mut c,
        "CREATE FULLTEXT INDEX optFt FOR (n:Opt) ON EACH [n.t]",
    );
    run(
        &mut c,
        "CREATE FULLTEXT INDEX optFr FOR ()-[r:R]-() ON EACH [r.note]",
    );
    run(&mut c, "CALL db.awaitIndexes()");
    c
}

/// The `options` map both fulltext procedures document (REL8-2031 #75):
/// `skip`/`limit` page the ranking exactly as Neo4j's do — the issue's
/// `{limit: 2}` → `[1, 3]`, `{skip: 1, limit: 1}` → `[3]`, `{}` → the whole
/// `[1, 3, 2]` — and `analyzer` selects the query-side analyzer exactly as
/// `SEARCH ... WITH ANALYZER` does (ADR-0672 §4). An unknown key, a bad value
/// and an unknown analyzer name are typed rejections, never silently ignored
/// options.
#[test]
fn the_procedure_options_map_pages_the_ranking() {
    let mut c = options_store();
    let ranked = |c: &mut relate::Connection, options: &str| {
        first_column(&run(
            c,
            &format!(
                "CALL db.index.fulltext.queryNodes('optFt', 'apple'{options}) \
                 YIELD node RETURN node.id AS id"
            ),
        ))
    };
    let ids = |values: &[i64]| values.iter().copied().map(Value::Int).collect::<Vec<_>>();
    assert_eq!(ranked(&mut c, ""), ids(&[1, 3, 2]));
    assert_eq!(ranked(&mut c, ", {}"), ids(&[1, 3, 2]));
    assert_eq!(ranked(&mut c, ", {limit: 2}"), ids(&[1, 3]));
    assert_eq!(ranked(&mut c, ", {skip: 1, limit: 1}"), ids(&[3]));
    assert_eq!(ranked(&mut c, ", {skip: 2}"), ids(&[2]));

    // The relationship side: two `apple` notes, one under `{limit: 1}`.
    let result = run(
        &mut c,
        "CALL db.index.fulltext.queryRelationships('optFr', 'apple', {limit: 1}) \
         YIELD relationship RETURN count(*) AS hits",
    );
    assert_eq!(first_column(&result), [Value::Int(1)]);

    // The `analyzer` key runs the query side's analyzer, exactly as SEARCH's
    // WITH ANALYZER does: `the` is a stop word under `english` and a term
    // under the index's default analyzer.
    let the = |c: &mut relate::Connection, options: &str| {
        first_column(&run(
            c,
            &format!(
                "CALL db.index.fulltext.queryNodes('optFt', 'the'{options}) \
                 YIELD node RETURN node.id AS id"
            ),
        ))
    };
    assert_eq!(the(&mut c, ""), ids(&[5]));
    assert_eq!(the(&mut c, ", {analyzer: 'english'}"), Vec::<Value>::new());
    // An unknown analyzer name is the analyzer reader's own rejection.
    let error = common::refused_literal(
        &mut c,
        "CALL db.index.fulltext.queryNodes('optFt', 'apple', {analyzer: 'nope'}) \
         YIELD node RETURN node.id",
    );
    assert!(error.to_string().contains("analyzer name"), "{error}");

    for options in [", {count: 1}", ", {limit: -1}", ", {limit: 'two'}"] {
        let statement = format!(
            "CALL db.index.fulltext.queryNodes('optFt', 'apple'{options}) \
             YIELD node RETURN node.id"
        );
        let error = common::refused_literal(&mut c, &statement);
        assert!(error.to_string().contains("option"), "{options}: {error}");
    }
}
