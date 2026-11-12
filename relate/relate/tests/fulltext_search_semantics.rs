//! CIP-225 fulltext SEARCH semantics and the `db.index.fulltext.query*`
//! procedures, pinned against live Neo4j 2026.10.1 (enterprise, scratch
//! database): the Lucene query syntax passes through (`AND`/`NOT`, a
//! lowercase `and` staying a term), the hit order is live's BM25 ranking
//! (the calibrated score values are asserted directly), `SKIP`/`OFFSET`
//! page that ranked list, `WITH ANALYZER` is a per-row expression, and the
//! procedures run the same scanner (multi-term OR over every indexed
//! property).

mod common;

use common::assert_syntax_error;
use common::connection;
use common::first_column_strings as titles;
use common::run;
use relate::Value;

fn scored(result: &relate::QueryResult) -> Vec<(String, f64)> {
    result
        .rows
        .iter()
        .map(|row| match (&row[0], &row[1]) {
            (Value::String(title), Value::Float(score)) => (title.to_string(), *score),
            other => panic!("expected (STRING, FLOAT), got {other:?}"),
        })
        .collect()
}

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

fn sorted_names(hits: &[(String, f64)]) -> Vec<String> {
    let mut names: Vec<String> = hits.iter().map(|(name, _)| name.clone()).collect();
    names.sort();
    names
}

fn search_names(c: &mut relate::Connection, query: &str) -> Vec<(String, f64)> {
    let result = run(
        c,
        &format!(
            "MATCH (p:Person) \
             SEARCH p IN (FULLTEXT INDEX personText FOR '{query}' LIMIT 10) \
             SCORE AS score RETURN p.name AS name, score ORDER BY score DESC, name ASC"
        ),
    );
    scored(&result)
}

fn procedure_names(c: &mut relate::Connection, query: &str) -> Vec<String> {
    let result = run(
        c,
        &format!(
            "CALL db.index.fulltext.queryNodes('personText', '{query}') \
             YIELD node, score RETURN node.name AS name ORDER BY score DESC, name ASC"
        ),
    );
    titles(&result)
}

/// `term~N` matches near terms (a transposition or one insertion away),
/// ranks the exact spelling above the neighbour, and `~0` / no suffix are
/// exact — through the SEARCH clause and the procedure alike.
#[test]
fn fuzzy_term_matches_near_spellings() {
    let mut c = people_store();
    let hits = search_names(&mut c, "kolleger~1");
    assert_eq!(
        hits.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["Kolleger", "Kollegger"]
    );
    assert!(hits[0].1 > hits[1].1, "{hits:?}");
    assert_eq!(
        procedure_names(&mut c, "kolleger~1"),
        ["Kolleger", "Kollegger"]
    );

    for exact in ["kolleger~0", "kolleger"] {
        assert_eq!(
            search_names(&mut c, exact)
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>(),
            ["Kolleger"],
            "{exact}"
        );
    }
    // A bare `~` is Lucene's default distance 2.
    assert_eq!(search_names(&mut c, "kollger~").len(), 2);
    // The suffix digit is never a term: `~1` must not require a token `1`.
    assert_eq!(search_names(&mut c, "kolleger~1 OR zzz").len(), 2);
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
        let found: Vec<String> = search_names(&mut c, query)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(found, expected, "SEARCH {query}");
        let mut via_procedure = procedure_names(&mut c, query);
        via_procedure.sort();
        let mut expected_sorted = expected.clone();
        expected_sorted.sort();
        assert_eq!(via_procedure, expected_sorted, "procedure {query}");
    }
}

/// `term^B` scales the clause's score without changing which rows match.
#[test]
fn boost_reorders_without_changing_the_match_set() {
    let mut c = people_store();
    let plain = search_names(&mut c, "consultant lamp");
    let boosted = search_names(&mut c, "consultant^10 lamp");
    assert_eq!(sorted_names(&plain), sorted_names(&boosted));
    // `lamp` is rarer than `consultant`, so Anna leads until `consultant`
    // is boosted past it.
    assert_eq!(plain[0].0, "Anna", "{plain:?}");
    assert_ne!(boosted[0].0, "Anna", "{boosted:?}");
    // Zero boost keeps the match, scoring 0.
    let zero = search_names(&mut c, "lamp^0");
    assert_eq!(zero.len(), 1);
    assert_eq!(zero[0].1, 0.0);
    // Boost composes with fuzzy.
    let both = search_names(&mut c, "kolleger~1^2");
    let single = search_names(&mut c, "kolleger~1");
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

/// Each query is a syntax error through the SEARCH clause and through
/// `queryNodes` on `(label, index)`.
fn assert_syntax_error_on_both_paths(
    c: &mut relate::Connection,
    (label, index): (&str, &str),
    queries: &[&str],
) {
    for query in queries {
        let error = c
            .query(&format!(
                "MATCH (p:{label}) \
                 SEARCH p IN (FULLTEXT INDEX {index} FOR '{query}' LIMIT 5) RETURN p.name"
            ))
            .expect_err(query);
        assert_syntax_error(&error);
        let error = c
            .query(&format!(
                "CALL db.index.fulltext.queryNodes('{index}', '{query}') YIELD node \
                 RETURN node.name"
            ))
            .expect_err(query);
        assert_syntax_error(&error);
    }
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
    let hits = |c: &mut relate::Connection, query: &str| -> Vec<(String, f64)> {
        let result = run(
            c,
            &format!(
                "CALL db.index.fulltext.queryNodes('nameText', '{query}') \
                 YIELD node, score RETURN node.name AS name, score \
                 ORDER BY score DESC, name ASC"
            ),
        );
        scored(&result)
    };
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

/// The speaker-template store: the same accented names under a
/// `standard-folding` index (`Folded`) and the default one (`Plain`),
/// mirroring the live Neo4j 2026.09-enterprise probe corpus.
fn speaker_store() -> relate::Connection {
    let mut c = connection();
    for (label, options) in [
        (
            "Folded",
            " OPTIONS {indexConfig: {`fulltext.analyzer`: 'standard-folding'}}",
        ),
        ("Plain", ""),
    ] {
        run(
            &mut c,
            &format!(
                "CREATE FULLTEXT INDEX idx{label} IF NOT EXISTS \
                 FOR (p:{label}) ON EACH [p.name]{options}"
            ),
        );
    }
    run(&mut c, "CALL db.awaitIndexes()");
    for label in ["Folded", "Plain"] {
        run(
            &mut c,
            &format!(
                "UNWIND ['Eifrém', 'Kollegger', 'Kolleggers', 'Kollegen', 'Kollegium', \
                 'Andreas Satéras', 'Müller', 'Zoë', 'naïve', 'Æther', 'Straße', 'Łódź', \
                 'Eifrem plain', 'Crème brûlée'] AS n CREATE (:{label} {{name: n}})"
            ),
        );
    }
    c
}

fn speaker_search(c: &mut relate::Connection, label: &str, query: &str) -> Vec<(String, f64)> {
    let result = run(
        c,
        &format!(
            "MATCH (p:{label}) \
             SEARCH p IN (FULLTEXT INDEX idx{label} FOR '{query}' LIMIT 50) \
             SCORE AS score RETURN p.name AS name, score ORDER BY score DESC, name ASC"
        ),
    );
    scored(&result)
}

fn speaker_procedure(c: &mut relate::Connection, label: &str, query: &str) -> Vec<(String, f64)> {
    let result = run(
        c,
        &format!(
            "CALL db.index.fulltext.queryNodes('idx{label}', '{query}') \
             YIELD node, score RETURN node.name AS name, score ORDER BY score DESC, name ASC"
        ),
    );
    scored(&result)
}

/// Both entry points, which must agree; returns the sorted names.
fn speakers(c: &mut relate::Connection, label: &str, query: &str) -> Vec<String> {
    let clause = speaker_search(c, label, query);
    assert_eq!(
        sorted_names(&clause),
        sorted_names(&speaker_procedure(c, label, query)),
        "{label} {query}"
    );
    sorted_names(&clause)
}

/// The reported cases: a `standard-folding` index folds diacritics at index
/// and query time (`eifrem` finds `Eifrém`, `andreas sateras` finds
/// `Andreas Satéras`), where the default index keeps them distinct.
#[test]
fn a_folding_index_folds_stored_tokens_and_query_terms() {
    let mut c = speaker_store();
    for (query, expected) in [
        ("eifrem", vec!["Eifrem plain", "Eifrém"]),
        ("EIFRÉM", vec!["Eifrem plain", "Eifrém"]),
        ("andreas sateras", vec!["Andreas Satéras"]),
        ("andreas satéras", vec!["Andreas Satéras"]),
        ("muller", vec!["Müller"]),
        ("zoe naive", vec!["Zoë", "naïve"]),
        ("aether", vec!["Æther"]),
        ("strasse", vec!["Straße"]),
        ("lodz", vec!["Łódź"]),
        ("creme brulee", vec!["Crème brûlée"]),
    ] {
        assert_eq!(speakers(&mut c, "Folded", query), expected, "{query}");
    }
    // The default analyzer does not fold either side.
    assert_eq!(speakers(&mut c, "Plain", "eifrem"), ["Eifrem plain"]);
    assert_eq!(speakers(&mut c, "Plain", "eifrém"), ["Eifrém"]);
    assert!(speakers(&mut c, "Plain", "andreas sateras").contains(&"Andreas Satéras".to_string()));
    assert!(speakers(&mut c, "Plain", "muller").is_empty());
    // `WITH ANALYZER` folds the query side only: stored tokens stay as
    // indexed, so a folded query term misses an unfolded index.
    let result = run(
        &mut c,
        "MATCH (p:Plain) SEARCH p IN (FULLTEXT INDEX idxPlain FOR 'muller' \
         WITH ANALYZER 'standard-folding' LIMIT 5) RETURN p.name",
    );
    assert!(result.rows.is_empty());
}

/// Fuzzy terms are lowercased but never folded (live probes): `eifrém~0`
/// misses the folded `eifrem` while `eifrém~1` is one edit away. Wildcard
/// terms are folded under a folding index — a deliberate deviation from live
/// (ADR-0583), so `Müll*`, `mull*`, `MÜLL*` and `EIFRÉ*` all find the folded
/// tokens — and keep their accents on an index that does not fold.
#[test]
fn fuzzy_terms_are_not_folded_but_wildcards_are() {
    let mut c = speaker_store();
    assert!(speakers(&mut c, "Folded", "eifrém~0").is_empty());
    assert_eq!(
        speakers(&mut c, "Folded", "eifrém~1"),
        ["Eifrem plain", "Eifrém"]
    );
    assert_eq!(speakers(&mut c, "Folded", "muller~0"), ["Müller"]);
    for query in ["mull*", "Müll*", "MÜLL*", "m?ller", "*üller"] {
        assert_eq!(speakers(&mut c, "Folded", query), ["Müller"], "{query}");
    }
    assert_eq!(
        speakers(&mut c, "Folded", "EIFRÉ*"),
        ["Eifrem plain", "Eifrém"]
    );
    assert_eq!(speakers(&mut c, "Folded", "sater*"), ["Andreas Satéras"]);
    assert_eq!(speakers(&mut c, "Folded", "Satér*"), ["Andreas Satéras"]);
    // `ß` folds to `ss`: the literal folds, and `?` counts folded characters.
    assert_eq!(speakers(&mut c, "Folded", "straß*"), ["Straße"]);
    assert_eq!(speakers(&mut c, "Folded", "stra??e"), ["Straße"]);
    assert!(speakers(&mut c, "Folded", "stra?e").is_empty());
    // Control: the default index stores accented tokens and does not fold
    // wildcards, so the accent is what matches.
    assert_eq!(speakers(&mut c, "Plain", "Müll*"), ["Müller"]);
    assert_eq!(speakers(&mut c, "Plain", "MÜLL*"), ["Müller"]);
    assert_eq!(speakers(&mut c, "Plain", "EIFRÉ*"), ["Eifrém"]);
    assert!(speakers(&mut c, "Plain", "mull*").is_empty());
    assert!(speakers(&mut c, "Plain", "sater*").is_empty());
    assert!(speakers(&mut c, "Plain", "straß*").contains(&"Straße".to_string()));
    assert!(speakers(&mut c, "Plain", "strass*").is_empty());
}

/// The reported prefix case and the wildcard forms: `*` runs, `?` single
/// characters, leading wildcards, field filters, case-insensitivity.
#[test]
fn wildcard_terms_match_prefixes_infixes_and_single_characters() {
    let mut c = speaker_store();
    let kolleg = ["Kollegen", "Kollegger", "Kolleggers", "Kollegium"];
    for (query, expected) in [
        ("Kollegg*", vec!["Kollegger", "Kolleggers"]),
        ("KOLLEGG*", vec!["Kollegger", "Kolleggers"]),
        ("kolleg*", kolleg.to_vec()),
        ("kolleg**", kolleg.to_vec()),
        ("*olleg*", kolleg.to_vec()),
        ("*gger", vec!["Kollegger"]),
        ("k?llegger", vec!["Kollegger"]),
        ("?ollegger", vec!["Kollegger"]),
        ("kolleg?er", vec!["Kollegger"]),
        ("koll*gger", vec!["Kollegger"]),
        ("name:kolleg*", kolleg.to_vec()),
        ("nosuch:kolleg*", vec![]),
        ("zzz*", vec![]),
    ] {
        for label in ["Plain", "Folded"] {
            assert_eq!(speakers(&mut c, label, query), expected, "{label} {query}");
        }
    }
    // `*` and `*:*` match every entity with a token; `?` only one-character
    // tokens (none here).
    for query in ["*", "**", "name:*", "*:*"] {
        assert_eq!(speakers(&mut c, "Plain", query).len(), 14, "{query}");
    }
    assert!(speakers(&mut c, "Plain", "?").is_empty());
}

/// Wildcard terms score a constant 1.0 (times the boost) however long the
/// entity or how many terms match, and sum with the other clauses.
#[test]
fn wildcard_scoring_is_constant_and_composes_with_other_clauses() {
    let mut c = speaker_store();
    for (query, expected) in [("kolleg*", 1.0), ("kolleg*^3", 3.0), ("kolleg*^0", 0.0)] {
        for hits in [
            speaker_search(&mut c, "Plain", query),
            speaker_procedure(&mut c, "Plain", query),
        ] {
            assert_eq!(hits.len(), 4, "{query}");
            assert!(hits.iter().all(|(_, score)| *score == expected), "{query}");
        }
    }
    // An exact term adds its BM25 on top of the constant.
    let hits = speaker_search(&mut c, "Plain", "kolleg* eifrem");
    assert_eq!(hits.len(), 5);
    let (name, score) = hits.last().expect("a hit");
    assert_eq!(name, "Eifrem plain");
    assert!(*score > 0.0 && *score < 1.0, "{score}");
    // A fuzzy suffix on a wildcard is ignored, as live.
    assert_eq!(
        speaker_search(&mut c, "Plain", "kolleg*~1"),
        speaker_search(&mut c, "Plain", "kolleg*")
    );
    assert_eq!(
        speakers(&mut c, "Plain", "kolleger~1 kolleg*"),
        ["Kollegen", "Kollegger", "Kolleggers", "Kollegium"]
    );
}

#[test]
fn wildcards_obey_and_not_and_clause_groups() {
    let mut c = speaker_store();
    for (query, expected) in [
        (
            "kolleg* AND NOT kollegger",
            vec!["Kollegen", "Kolleggers", "Kollegium"],
        ),
        ("kolleg* AND eifrem", vec![]),
        ("NOT kolleg*", vec![]),
        ("kollegg* AND NOT kolleggers", vec!["Kollegger"]),
        ("(kollegg*) OR zoë", vec!["Kollegger", "Kolleggers", "Zoë"]),
    ] {
        assert_eq!(speakers(&mut c, "Plain", query), expected, "{query}");
    }
}

/// An escaped wildcard character is plain text the analyzer drops
/// (`kollegger\*` is the term `kollegger`, `kolleg\*` the term `kolleg`), and
/// a quoted word is analyzed as text rather than expanded.
#[test]
fn escaped_and_quoted_wildcards_are_ordinary_text() {
    let mut c = speaker_store();
    assert_eq!(speakers(&mut c, "Plain", r"kollegger\\*"), ["Kollegger"]);
    assert!(speakers(&mut c, "Plain", r"kolleg\\*").is_empty());
    assert!(speakers(&mut c, "Plain", r#"\"kolleg*\""#).is_empty());
    assert_eq!(
        speakers(&mut c, "Plain", r#"\"kollegger*\""#),
        ["Kollegger"]
    );
}

/// A wildcard expands over the whole vocabulary: Lucene's 1024-clause limit
/// does not apply (live: 3000 distinct terms, 3000 hits).
#[test]
fn wildcard_expansion_is_not_capped() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE FULLTEXT INDEX idxBig FOR (n:Big) ON EACH [n.name]",
    );
    run(&mut c, "CALL db.awaitIndexes()");
    run(
        &mut c,
        "UNWIND range(0, 1999) AS i CREATE (:Big {name: 'tok' + toString(i) + ' common'})",
    );
    let hits = run(
        &mut c,
        "CALL db.index.fulltext.queryNodes('idxBig', 'tok*') YIELD score RETURN score",
    );
    assert_eq!(hits.rows.len(), 2000);
    let hits = run(
        &mut c,
        "MATCH (n:Big) SEARCH n IN (FULLTEXT INDEX idxBig FOR 't*k1?9' LIMIT 100) \
         RETURN n.name",
    );
    assert_eq!(hits.rows.len(), 10);
}

#[test]
fn wildcard_suffix_errors_are_syntax_errors() {
    let mut c = speaker_store();
    assert_syntax_error_on_both_paths(
        &mut c,
        ("Plain", "idxPlain"),
        &["kolleg*^x", "kolleg*^", "kolleg*^-1", "~1", "kolleg*^2~1"],
    );
}
