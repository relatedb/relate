//! Hybrid search: the `COMBINE` source specification behind the
//! ADR-0371 `hybrid-search` preview gate. The film corpus (synopses,
//! embeddings, motif embeddings) and every query below are authored
//! locally for this suite; the WRRF-fused expectations were recomputed
//! by running these fixtures through this engine and are pinned to its
//! deterministic rank orders.

mod common;

use common::first_column_strings as titles;
use relate::Value;

/// The hybrid-search preview gate is process-level (ADR-0371:
/// `Database::open` resolves `RELATE_PREVIEW` once), and an
/// integration-test crate is its own process -- set it before any store
/// opens and keep every test in this crate behind the gate.
fn gated_connection() -> relate::Connection {
    // Fail loudly (the same contract as the preview fixture in
    // `relate/src/tests/preview.rs`) rather than run half-gated.
    common::set_preview_once("hybrid-search");
    common::connection()
}

/// Extracts a FLOAT cell from a result row.
fn float(value: &Value) -> f64 {
    let Value::Float(score) = value else {
        panic!("expected a FLOAT score, got {value:?}");
    };
    *score
}

/// The corpus: eight films with a synopsis and a 3-dimensional stand-in
/// embedding, plus the vector and fulltext indexes over them.
fn film_store() -> relate::Connection {
    let mut c = gated_connection();
    c.query(
        "CREATE FULLTEXT INDEX filmSynopsisFulltext IF NOT EXISTS \
         FOR (film:Film) ON EACH [film.synopsis]",
    )
    .unwrap();
    c.query(
        "CREATE VECTOR INDEX filmSynopsisEmbeddings IF NOT EXISTS \
         FOR (film:Film) ON film.embedding \
         OPTIONS {indexConfig: {`vector.dimensions`: 3, \
         `vector.similarity_function`: 'cosine'}}",
    )
    .unwrap();
    c.query(
        "CREATE (f1:Film {id: 'film-signal-to-titan', title: 'Signal to Titan', \
           synopsis: 'A lunar freighter relay officer strands alone on a beacon \
                      station after a solar storm, routing her distress signal \
                      through the beacon network of the outer moons.', \
           embedding: [0.88, 0.46, 0.20]}), \
         (f2:Film {id: 'film-long-dark-of-europa', title: 'The Long Dark of Europa', \
           synopsis: 'An ice crew on Europa loses the lunar freighter relay in a \
                      quake and crosses the dark side to relight their beacon.', \
           embedding: [0.80, 0.55, 0.25]}), \
         (f3:Film {id: 'film-debris-halo', title: 'Debris Halo', \
           synopsis: 'A salvage team clears a wreckage halo after two docks \
                      collide, quarrelling over the last log of the lost hauler.', \
           embedding: [0.83, 0.45, 0.31]}), \
         (f4:Film {id: 'film-freighter-of-the-quiet-sea', \
           title: 'Freighter of the Quiet Sea', \
           synopsis: 'A cargo pilot hauling ore between the orbital docks \
                      remembers her first lunar run, when a dead beacon nearly \
                      scattered the convoy across the quiet sea of craters.', \
           embedding: [0.76, 0.58, 0.28]}), \
         (f5:Film {id: 'film-cartographers-daughter', \
           title: \"The Cartographer's Daughter\", \
           synopsis: 'A mapmaker\u{2019}s daughter inherits a half-finished atlas \
                      of the outer moons and finishes his survey among the \
                      shepherd satellites.', \
           embedding: [0.60, 0.70, 0.35]}), \
         (f6:Film {id: 'film-nine-docks-of-mars', title: 'Nine Docks of Mars', \
           synopsis: 'A dock master juggles nine rival crews, a stowaway cat, \
                      and a lunar quarantine that closes her busiest pier.', \
           embedding: [0.55, 0.72, 0.40]}), \
         (f7:Film {id: 'film-ballad-of-the-ice-miners', \
           title: 'Ballad of the Ice Miners', \
           synopsis: 'Ice miners on the south pole sing of the year the bore \
                      collapsed and the whole town dug out one family at a time.', \
           embedding: [0.40, 0.75, 0.50]}), \
         (f8:Film {id: 'film-harvest-of-the-belt', title: 'Harvest of the Belt', \
           synopsis: 'A widow captains her late husband\u{2019}s ore harvester \
                      through the belt\u{2019}s spring swarm, bargaining for a \
                      berth at the far station.', \
           embedding: [0.20, 0.80, 0.55]})",
    )
    .unwrap();
    c
}

/// The weighted vector + fulltext hybrid over the corpus, weights
/// 0.7/0.3, constant 60, fused LIMIT 5. The expected fused order was
/// read off this engine's rows for these fixtures.
#[test]
fn combine_fuses_vector_and_fulltext_with_wrrf() {
    let mut c = film_store();
    let result = c
        .query(
            "MATCH (node) \
             SEARCH node IN ( \
               COMBINE ( \
                 VECTOR INDEX filmSynopsisEmbeddings \
                   FOR [0.85, 0.50, 0.15] LIMIT 10 WEIGHT 0.7, \
                 FULLTEXT INDEX filmSynopsisFulltext \
                   FOR 'lunar freighter relay beacon' LIMIT 10 WEIGHT 0.3 \
               ) USING WRRF WITH CONSTANT 60 LIMIT 5 \
             ) SCORE AS score \
             RETURN node.title AS title ORDER BY score DESC",
        )
        .unwrap();
    assert_eq!(
        titles(&result),
        vec![
            "Signal to Titan",
            "The Long Dark of Europa",
            "Freighter of the Quiet Sea",
            "Nine Docks of Mars",
            "Debris Halo",
        ]
    );
}

/// Omitted WEIGHTs default to 1 and an omitted WITH CONSTANT defaults to
/// 60 (unweighted RRF), with no fused LIMIT. The fused order was read
/// off this engine's rows for these fixtures.
#[test]
fn combine_defaults_weight_one_and_constant_sixty() {
    let mut c = film_store();
    let result = c
        .query(
            "MATCH (node) \
             SEARCH node IN ( \
               COMBINE ( \
                 VECTOR INDEX filmSynopsisEmbeddings \
                   FOR [0.85, 0.50, 0.15] LIMIT 10, \
                 FULLTEXT INDEX filmSynopsisFulltext \
                   FOR 'lunar freighter relay beacon' LIMIT 10 \
               ) USING WRRF \
             ) SCORE AS score \
             RETURN node.title AS title ORDER BY score DESC LIMIT 5",
        )
        .unwrap();
    assert_eq!(
        titles(&result),
        vec![
            "Signal to Titan",
            "The Long Dark of Europa",
            "Freighter of the Quiet Sea",
            "Nine Docks of Mars",
            "Debris Halo",
        ]
    );
}

/// The fused `SCORE AS` column is FLOAT NOT NULL: two sources both
/// ranking an entity first contribute exactly
/// `0.7/(60+1) + 0.3/(60+1) = 1/61`.
#[test]
fn combine_score_is_the_wrrf_sum() {
    let mut c = film_store();
    let result = c
        .query(
            "MATCH (node) \
             SEARCH node IN ( \
               COMBINE ( \
                 VECTOR INDEX filmSynopsisEmbeddings \
                   FOR [0.85, 0.50, 0.15] LIMIT 10 WEIGHT 0.7, \
                 FULLTEXT INDEX filmSynopsisFulltext \
                   FOR 'lunar freighter relay beacon' LIMIT 10 WEIGHT 0.3 \
               ) USING WRRF WITH CONSTANT 60 \
             ) SCORE AS score \
             RETURN score ORDER BY score DESC LIMIT 1",
        )
        .unwrap();
    let top = float(&result.rows[0][0]);
    assert!((top - 1.0 / 61.0).abs() < 1e-12, "top score {top}");
}

/// A source an entity is missing from contributes nothing from that
/// source: Debris Halo is the vector search's rank 3 (0-based 2) but
/// matches no fulltext term, so its fused score is exactly the vector
/// contribution `1/(60+2+1)`.
#[test]
fn combine_missing_source_contributes_zero() {
    let mut c = film_store();
    let result = c
        .query(
            "MATCH (node) \
             SEARCH node IN ( \
               COMBINE ( \
                 VECTOR INDEX filmSynopsisEmbeddings \
                   FOR [0.85, 0.50, 0.15] LIMIT 10, \
                 FULLTEXT INDEX filmSynopsisFulltext \
                   FOR 'lunar freighter relay beacon' LIMIT 10 \
               ) USING WRRF \
             ) SCORE AS score \
             RETURN node.title AS title, score",
        )
        .unwrap();
    let halo = result
        .rows
        .iter()
        .find(|row| matches!(&row[0], Value::String(t) if t.as_ref() == "Debris Halo"))
        .expect("Debris Halo rides the vector source alone");
    let score = float(&halo[1]);
    assert!((score - 1.0 / 63.0).abs() < 1e-12, "Halo score {score}");
}

/// A nested COMBINE source fuses its own sources first, then ranks into
/// the outer fusion under its own weight (here the motif-embedding
/// vector source fused with the fulltext source inside, the synopsis
/// embedding outside). The head of the fused order was read off this
/// engine's rows for these fixtures.
#[test]
fn combine_nests_a_combine_source() {
    let mut c = film_store();
    c.query(
        "CREATE VECTOR INDEX filmMotifEmbeddings IF NOT EXISTS \
         FOR (film:Film) ON film.motifEmbedding \
         OPTIONS {indexConfig: {`vector.dimensions`: 3, \
         `vector.similarity_function`: 'cosine'}}",
    )
    .unwrap();
    c.query(
        "MATCH (f:Film) WHERE f.id IN ['film-signal-to-titan', 'film-long-dark-of-europa', \
         'film-debris-halo', 'film-freighter-of-the-quiet-sea', 'film-cartographers-daughter', \
         'film-nine-docks-of-mars', 'film-ballad-of-the-ice-miners', 'film-harvest-of-the-belt'] \
         SET f.motifEmbedding = CASE f.id \
           WHEN 'film-signal-to-titan' THEN [0.10, 0.95, 0.05] \
           WHEN 'film-long-dark-of-europa' THEN [0.15, 0.80, 0.10] \
           WHEN 'film-debris-halo' THEN [0.85, 0.20, 0.10] \
           WHEN 'film-freighter-of-the-quiet-sea' THEN [0.30, 0.70, 0.15] \
           WHEN 'film-cartographers-daughter' THEN [0.05, 0.60, 0.30] \
           WHEN 'film-nine-docks-of-mars' THEN [0.20, 0.75, 0.05] \
           WHEN 'film-ballad-of-the-ice-miners' THEN [0.40, 0.60, 0.10] \
           WHEN 'film-harvest-of-the-belt' THEN [0.60, 0.35, 0.10] END",
    )
    .unwrap();
    let result = c
        .query(
            "MATCH (node) \
             SEARCH node IN ( \
               COMBINE ( \
                 VECTOR INDEX filmSynopsisEmbeddings \
                   FOR [0.55, 0.70, 0.40] LIMIT 10 WEIGHT 1.0, \
                 COMBINE ( \
                   VECTOR INDEX filmMotifEmbeddings \
                     FOR [0.35, 0.65, 0.05] LIMIT 10 WEIGHT 1.3, \
                   FULLTEXT INDEX filmSynopsisFulltext \
                     FOR 'lunar freighter relay beacon' LIMIT 10 WEIGHT 0.7 \
                 ) USING WRRF WEIGHT 0.7 \
               ) USING WRRF \
             ) SCORE AS score \
             RETURN node.title AS title ORDER BY score DESC LIMIT 5",
        )
        .unwrap();
    let got = titles(&result);
    assert_eq!(got.len(), 5, "{got:?}");
    assert_eq!(got[0], "Nine Docks of Mars", "{got:?}");
    assert_eq!(got[1], "Freighter of the Quiet Sea", "{got:?}");
}

fn assert_non_numeric_combine_input_is_rejected(weight: &str, constant: &str, clause: &str) {
    let mut c = film_store();
    let error = common::err_text(c.query(&format!(
        "MATCH (node) \
             SEARCH node IN ( \
               COMBINE ( \
                 VECTOR INDEX filmSynopsisEmbeddings \
                   FOR [0.85, 0.50, 0.15] LIMIT 10 WEIGHT {weight}, \
                 FULLTEXT INDEX filmSynopsisFulltext \
                   FOR 'lunar freighter relay beacon' LIMIT 10 WEIGHT 0.3 \
               ) USING WRRF WITH CONSTANT {constant} \
             ) SCORE AS score \
             RETURN node, score"
    )));
    assert!(
        error.contains(&format!(
            "Expected the value to be of type INTEGER NOT NULL or FLOAT NOT NULL, \
             but was of type STRING NOT NULL ({clause})"
        )),
        "{error}"
    );
}

/// A WEIGHT that is not INTEGER NOT NULL or FLOAT NOT NULL fails with
/// the invalid-type error.
#[test]
fn combine_rejects_a_non_numeric_weight() {
    assert_non_numeric_combine_input_is_rejected("'not a number'", "60", "SEARCH WEIGHT");
}

/// A WITH CONSTANT that is not INTEGER NOT NULL or FLOAT NOT NULL fails
/// the same way.
#[test]
fn combine_rejects_a_non_numeric_constant() {
    assert_non_numeric_combine_input_is_rejected("0.7", "'not a number'", "SEARCH CONSTANT");
}

/// A WEIGHT expression may not reference the search clause's own binding
/// variable.
#[test]
fn combine_rejects_a_self_referencing_weight() {
    let mut c = film_store();
    let error = common::err_text(c.query(
        "MATCH (node) \
             SEARCH node IN ( \
               COMBINE ( \
                 VECTOR INDEX filmSynopsisEmbeddings \
                   FOR [0.85, 0.50, 0.15] LIMIT 10 WEIGHT 0.3 * node.weight, \
                 FULLTEXT INDEX filmSynopsisFulltext \
                   FOR 'lunar freighter relay beacon' LIMIT 10 WEIGHT 0.3 \
               ) USING WRRF WITH CONSTANT 60 \
             ) SCORE AS score \
             RETURN node, score",
    ));
    assert!(
        error.contains(
            "SelfReferencingSearch: the WEIGHT or CONSTANT expression \
             may not depend on the search clause binding variable 'node'"
        ),
        "{error}"
    );
}

/// A single weighted source fails the parse with the missing-source
/// error.
#[test]
fn combine_requires_two_sources() {
    let mut c = film_store();
    let error = common::err_text(c.query(
        "MATCH (node) \
             SEARCH node IN ( \
               COMBINE ( \
                 VECTOR INDEX filmSynopsisEmbeddings \
                   FOR [0.85, 0.50, 0.15] LIMIT 10 WEIGHT 0.3 \
               ) USING WRRF WITH CONSTANT 60 \
             ) SCORE AS score \
             RETURN node, score",
    ));
    assert!(
        error.contains("The COMBINE clause requires two or more sources provided"),
        "{error}"
    );
}
