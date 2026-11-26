//! The vector index's `indexConfig` admission (LABS-708): the SEARCH
//! acceptance suites' Background creates its index with
//! `OPTIONS {indexConfig: {`vector.quantization.type`: 'none'}}` — the
//! key the ontology validator already accepts — so the CREATE must admit
//! it and the resulting index must be searchable.

use crate::common;

#[test]
fn quantization_type_none_index_is_searchable() {
    let mut c = common::connection();

    c.query(
        "CREATE VECTOR INDEX movieEmbeddings FOR (movie:Movie) ON movie.embedding \
         OPTIONS {indexConfig: {`vector.quantization.type`: 'none'}}",
    )
    .expect("the quantization type key is admitted");

    common::run(
        &mut c,
        "CREATE (:Movie {id: 'one', embedding: [1.0, 0.0]}), \
         (:Movie {id: 'two', embedding: [0.0, 1.0]})",
    );

    assert_eq!(
        common::rows(
            &mut c,
            "MATCH (movie) \
             SEARCH movie IN (VECTOR INDEX movieEmbeddings FOR [1.0, 0.0] LIMIT 5) \
             RETURN movie.id AS id
             ORDER BY movie.id",
        ),
        vec![vec![common::string("one")], vec![common::string("two")]]
    );
}
