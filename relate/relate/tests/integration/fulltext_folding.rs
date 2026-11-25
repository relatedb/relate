//! The fulltext folding and wildcard family: the speaker-template
//! store (a `standard-folding` index beside the default one) and the
//! accent-folding, fuzzy-vs-wildcard folding, wildcard syntax, scoring,
//! boolean composition and expansion-cap tests pinned against live
//! Neo4j (ADR-0583). The analyzer semantics and the procedure/SEARCH
//! agreement tests live in `fulltext_search_semantics.rs`; the ranked-hit
//! helpers live in `common`.

use crate::common;

use common::connection;
use common::fulltext::assert_syntax_error_on_both_paths;
use common::fulltext::fulltext_agree;
use common::fulltext::fulltext_search;
use common::fulltext::sorted_names;
use common::fulltext::AnalyzerOverride;
use common::run;

/// The speaker corpus — the fact every speaker-count assertion derives
/// from.
const SPEAKERS: [&str; 14] = [
    "Eifrém",
    "Kollegger",
    "Kolleggers",
    "Kollegen",
    "Kollegium",
    "Andreas Satéras",
    "Müller",
    "Zoë",
    "naïve",
    "Æther",
    "Straße",
    "Łódź",
    "Eifrem plain",
    "Crème brûlée",
];

/// The speaker store's two sides — `(label, index options)`: the one fact
/// the store builder and every both-sides assertion derive from.
const SIDES: [(&str, &str); 2] = [
    (
        "Folded",
        " OPTIONS {indexConfig: {`fulltext.analyzer`: 'standard-folding'}}",
    ),
    ("Plain", ""),
];

/// The speaker-template store: the same accented names under a
/// `standard-folding` index (`Folded`) and the default one (`Plain`),
/// mirroring the live Neo4j 2026.09-enterprise probe corpus. The two
/// sides are one table, iterated once for the indexes and once for the
/// corpus.
fn speaker_store() -> relate::Connection {
    let mut c = connection();
    for (label, options) in SIDES {
        run(
            &mut c,
            &format!(
                "CREATE FULLTEXT INDEX idx{label} IF NOT EXISTS \
                 FOR (p:{label}) ON EACH [p.name]{options}"
            ),
        );
    }
    run(&mut c, "CALL db.awaitIndexes()");
    let names = SPEAKERS
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ");
    for (label, _) in SIDES {
        run(
            &mut c,
            &format!("UNWIND [{names}] AS n CREATE (:{label} {{name: n}})"),
        );
    }
    c
}

/// Both entry points over the speaker store's `idx{label}` indexes — the
/// shared agreement helper through this store's naming: the ranked hits.
fn speaker_hits(c: &mut relate::Connection, label: &str, query: &str) -> Vec<(String, f64)> {
    fulltext_agree(c, label, &format!("idx{label}"), "name", 50, query)
}

/// The sorted names of the speaker store's agreement hits.
fn speakers(c: &mut relate::Connection, label: &str, query: &str) -> Vec<String> {
    sorted_names(&speaker_hits(c, label, query))
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
    assert!(fulltext_search(
        &mut c,
        "Plain",
        "idxPlain",
        "name",
        5,
        "muller",
        AnalyzerOverride::Named("standard-folding")
    )
    .is_empty());
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
        for (label, _) in SIDES {
            assert_eq!(speakers(&mut c, label, query), expected, "{label} {query}");
        }
    }
    // `*` and `*:*` match every entity with a token; `?` only one-character
    // tokens (none here).
    for query in ["*", "**", "name:*", "*:*"] {
        assert_eq!(
            speakers(&mut c, "Plain", query).len(),
            SPEAKERS.len(),
            "{query}"
        );
    }
    assert!(speakers(&mut c, "Plain", "?").is_empty());
}

/// Wildcard terms score a constant 1.0 (times the boost) however long the
/// entity or how many terms match, and sum with the other clauses.
#[test]
fn wildcard_scoring_is_constant_and_composes_with_other_clauses() {
    let mut c = speaker_store();
    for (query, expected) in [("kolleg*", 1.0), ("kolleg*^3", 3.0), ("kolleg*^0", 0.0)] {
        // The agreement helper asserts both entry points answer the same
        // ranked pairs; each scores the constant.
        let hits = speaker_hits(&mut c, "Plain", query);
        assert_eq!(hits.len(), 4, "{query}");
        assert!(hits.iter().all(|(_, score)| *score == expected), "{query}");
    }
    // An exact term adds its BM25 on top of the constant.
    let hits = speaker_hits(&mut c, "Plain", "kolleg* eifrem");
    assert_eq!(hits.len(), 5);
    let (name, score) = hits.last().expect("a hit");
    assert_eq!(name, "Eifrem plain");
    assert!(*score > 0.0 && *score < 1.0, "{score}");
    // A fuzzy suffix on a wildcard is ignored, as live.
    assert_eq!(
        speaker_hits(&mut c, "Plain", "kolleg*~1"),
        speaker_hits(&mut c, "Plain", "kolleg*")
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
        ("kollegg* OR zoë", vec!["Kollegger", "Kolleggers", "Zoë"]),
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
