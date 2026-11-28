//! The search-filter grammar's test bench, split from
//! `lower/search_filter.rs` (the `pathdb_tests` precedent): the vector
//! window's accepted/rejected shapes, and ADR-0714's fulltext grammar.

use super::*;

/// Lower the filters of the statement's one SEARCH clause directly, so
/// the accepted/rejected grammar shapes are pinned without a catalog.
/// The kind derives from the statement's own `VECTOR`/`FULLTEXT`
/// spelling through the one canonical `From` conversion the production
/// lowering reads — the per-kind grammar (ADR-0714) is what the callers
/// pin, and the spelling decides which of them runs.
fn lower_filters(query: &str) -> Result<Vec<SearchFilterPred>> {
    let stmt = relate_parser::parse(query).expect("parses");
    let search = stmt
        .clauses
        .iter()
        .find_map(|clause| match clause {
            relate_parser::Clause::Match(clause) => clause.search.as_ref(),
            _ => None,
        })
        .expect("expected a SEARCH subclause");
    let relate_parser::SearchSpec::Index(spec) = &search.spec else {
        panic!("expected a single index source")
    };
    let kind = SearchKind::from(spec.kind);
    lower_search_filters(
        &search.binding_var,
        kind,
        &spec.for_expr,
        spec.inner_where.as_ref(),
    )
}

fn expect_error(query: &str, expected: &str) {
    let error = lower_filters(query).expect_err("the predicate must be rejected");
    assert!(
        error.to_string().contains(expected),
        "expected {expected:?} in {error}"
    );
}

#[test]
fn property_predicates_may_name_the_for_entity_variable() {
    // `FOR m.embedding` names the index entity; the binding variable's
    // side is the candidate property whatever the operand order, so
    // `m.rating <= movie.rating` filters on `movie.rating >= m.rating`
    // with the row's `m.rating` as the RHS (live Neo4j 2026.10.1
    // returns the same rows as the `movie.rating >= m.rating` form).
    let filters = lower_filters(
        "MATCH (m:Movie) MATCH (movie:Movie) \
         SEARCH movie IN (VECTOR INDEX moviePlots FOR m.embedding \
         WHERE m.rating <= movie.rating LIMIT 2) RETURN movie.title",
    )
    .expect("accepted");
    let [SearchFilterPred::Comparison {
        property,
        op: SearchOp::Gte,
        rhs,
    }] = filters.as_slice()
    else {
        panic!("expected one comparison on the binding variable: {filters:?}")
    };
    assert_eq!(property, "rating");
    assert!(
        matches!(rhs, relate_ir::Expr::Property { .. }),
        "the row's `m.rating` is the RHS: {rhs:?}"
    );
}

#[test]
fn existence_predicates_are_accepted() {
    let filters = lower_filters(
        "MATCH (movie:Movie) \
         SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
         WHERE movie.rating IS NOT NULL AND movie.year IS NULL LIMIT 2) \
         RETURN movie.title",
    )
    .expect("accepted");
    assert!(matches!(
        filters.as_slice(),
        [
            SearchFilterPred::Exists { property: exists },
            SearchFilterPred::NotExists { property: not_exists },
        ] if exists == "rating" && not_exists == "year"
    ));
}

#[test]
fn in_predicates_cover_property_and_entity_sets() {
    let filters = lower_filters(
        "MATCH (movie:Movie) \
         SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
         WHERE movie.rating IN [7.6, 7.7] AND movie IN godFathers LIMIT 2) \
         RETURN movie.title",
    )
    .expect("accepted");
    assert!(matches!(
        filters.as_slice(),
        [
            SearchFilterPred::InSet { property, .. },
            SearchFilterPred::EntityInSet { .. },
        ] if property == "rating"
    ));
}

#[test]
fn for_entity_lhs_without_a_binding_variable_rhs_is_rejected() {
    // The FOR entity variable on the LHS is only the accepted
    // "independent property as LHS" form when the RHS names the
    // binding variable; a constant RHS is the wrong-variable error.
    expect_error(
        "MATCH (m:Movie) MATCH (movie:Movie) \
         SEARCH movie IN (VECTOR INDEX moviePlots FOR m.embedding \
         WHERE m.rating > 7 LIMIT 5) RETURN movie.title",
        "WrongVariableForVectorSearchFilter",
    );
}

#[test]
fn property_predicate_on_an_unrelated_variable_is_rejected() {
    // Without a FOR entity variable only the binding variable is
    // accepted (Neo4j 42I74).
    expect_error(
        "MATCH (m:Movie) MATCH (movie:Movie) \
         SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
         WHERE m.rating > 7 LIMIT 5) RETURN movie.title",
        "WrongVariableForVectorSearchFilter",
    );
}

#[test]
fn entity_set_predicate_must_name_the_binding_variable() {
    // Even the FOR entity variable is not an entity-set LHS: the filter
    // would apply to the wrong symbol (Neo4j 42I74).
    expect_error(
        "MATCH ()-[r {name: 'cs'}]->() MATCH ()-[rel]->() \
         SEARCH rel IN (VECTOR INDEX relIndex FOR r.vector \
         WHERE r IN [0] LIMIT 10) RETURN rel.id",
        "WrongVariableForVectorSearchFilter",
    );
}

#[test]
fn rhs_may_not_reference_the_lhs_variable() {
    expect_error(
        "MATCH (movie:Movie) \
         SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
         WHERE movie.year = movie.rating LIMIT 2) RETURN movie.title",
        "SelfReferencingVectorSearchFilter",
    );
}

#[test]
fn one_property_admits_a_single_predicate_or_one_bounded_range() {
    lower_filters(
        "MATCH (movie:Movie) \
         SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
         WHERE movie.year > 1945 AND movie.year < 2000 LIMIT 2) \
         RETURN movie.title",
    )
    .expect("a bounded range is two compatible predicates");
    for rejected in [
        "WHERE movie.year > 1945 AND movie.year > 2000",
        "WHERE movie.rating = 7.3 AND movie.rating = 7.4",
        "WHERE movie.rating = 7.3 AND movie.rating > 7",
        "WHERE movie.rating = 7.3 AND movie.year = 1999 AND movie.rating = 7.4",
    ] {
        expect_error(
            &format!(
                "MATCH (movie:Movie) SEARCH movie IN (VECTOR INDEX moviePlots \
                 FOR [1, 2, 3] {rejected} LIMIT 2) RETURN movie.title"
            ),
            "InvalidSearchFilterPredicate",
        );
    }
}

#[test]
fn more_than_one_entity_set_predicate_is_rejected() {
    expect_error(
        "MATCH (movie:Movie) \
         SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
         WHERE movie IN a AND movie IN b LIMIT 2) RETURN movie.title",
        "InvalidSearchFilterPredicate",
    );
}

/// ADR-0678: `id(<binding variable>) IN <expr>` is the entity-set
/// match filter's id-spelled twin; any other variable inside the
/// `id()` is the wrong-variable rejection, and the two membership
/// spellings share the at-most-one entity-filter budget.
#[test]
fn id_membership_predicates_name_the_binding_variable() {
    let filters = lower_filters(
        "MATCH ()-[r {name: 'cs'}]->() MATCH ()-[rel]->() \
         SEARCH rel IN (VECTOR INDEX relIndex FOR r.vector \
         WHERE id(rel) IN [10, 40] LIMIT 10) RETURN rel.id",
    )
    .expect("accepted");
    assert!(
        matches!(filters.as_slice(), [SearchFilterPred::IdInSet { .. }]),
        "{filters:?}"
    );
    expect_error(
        "MATCH ()-[r {name: 'cs'}]->() MATCH ()-[rel]->() \
         SEARCH rel IN (VECTOR INDEX relIndex FOR r.vector \
         WHERE id(r) IN [10] LIMIT 10) RETURN rel.id",
        "WrongVariableForVectorSearchFilter",
    );
    expect_error(
        "MATCH (movie:Movie) \
         SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
         WHERE id(movie.rating) IN [1] LIMIT 2) RETURN movie.title",
        "InvalidSearchFilterPredicate",
    );
    expect_error(
        "MATCH (movie:Movie) \
         SEARCH movie IN (VECTOR INDEX moviePlots FOR [1, 2, 3] \
         WHERE movie IN a AND id(movie) IN b LIMIT 2) RETURN movie.title",
        "InvalidSearchFilterPredicate",
    );
}

/// ADR-0714: a fulltext window's inner WHERE is the match filter only.
/// Every property predicate — a comparison, an existence form, a
/// property-in-set — is the reference server's invalid-predicate rejection, while
/// the two match-filter forms lower exactly as on a vector window, and
/// the vector window's property grammar is unchanged (the preceding
/// tests are that half: they all lower through the vector kind).
mod fulltext_grammar {
    use super::lower_filters;
    use super::*;

    pub(super) fn fulltext(predicate: &str) -> Result<Vec<SearchFilterPred>> {
        lower_filters(&format!(
            "MATCH (movie:Movie) SEARCH movie IN (FULLTEXT INDEX plots FOR 'adventure' \
             WHERE {predicate} LIMIT 10)"
        ))
    }

    /// The two match-filter forms lower as on a vector window.
    #[test]
    fn the_match_filter_forms_lower_on_a_fulltext_window() {
        let filters =
            fulltext("movie IN $candidates").expect("the match filter lowers on a fulltext window");
        assert!(matches!(
            filters.as_slice(),
            [SearchFilterPred::EntityInSet { .. }]
        ));
        let filters = fulltext("id(movie) IN $candidates")
            .expect("the id-spelled match filter lowers on a fulltext window");
        assert!(matches!(
            filters.as_slice(),
            [SearchFilterPred::IdInSet { .. }]
        ));
    }

    /// A property comparison is the invalid-predicate rejection, the
    /// reference server's 42I73 sentence with the term's own spelling — each row
    /// pins both the grammar (every one of these rejects) and the
    /// renderer: the engine's one float rendering policy (`7.0`, never
    /// the integer-reading `7`), the quoted message term (decoded
    /// newlines and quotes back as their visible escape spellings,
    /// printable text — non-ASCII included — passing through), the
    /// canonical identifier and parameter quoting (backtick-quoted
    /// names and `$`p 1`` re-parse as written, and an unsafe character
    /// inside a name renders escaped), parentheses, calls (the
    /// `DISTINCT` modifier included), and the all-or-nothing fallback —
    /// an unmodelled shape takes the whole-term placeholder, never a
    /// splice.
    #[test]
    fn a_property_comparison_is_the_invalid_predicate_rejection() {
        let cases = [
            ("movie.rating > 7", "movie.rating > 7"),
            ("movie.rating > 7.0", "movie.rating > 7.0"),
            ("movie.title = 'Alice\\nBob'", "movie.title = 'Alice\\nBob'"),
            ("movie.title = 'it\\'s'", "movie.title = 'it\\'s'"),
            ("movie.title = 'café'", "movie.title = 'café'"),
            ("movie.title = $`p 1`", "movie.title = $`p 1`"),
            ("movie.`weird prop` > 7", "movie.`weird prop` > 7"),
            ("movie.`a\nb` > 7", "movie.`a\\nb` > 7"),
            ("(movie.rating) > 7", "(movie.rating) > 7"),
            ("toFloat(movie.rating) > 7", "toFloat(movie.rating) > 7"),
            (
                "count(DISTINCT movie.rating) > 7",
                "count(DISTINCT movie.rating) > 7",
            ),
            ("movie.title = CASE WHEN true THEN 1 END", "<predicate>"),
        ];
        for (predicate, term) in cases {
            let error = fulltext(predicate).expect_err("a property predicate must be rejected");
            assert!(
                error.to_string().contains(&format!(
                    "invalid predicate for search with filters. The search filter \
                     predicate '{term}' is not supported, see \
                     documentation for all supported predicates."
                )),
                "predicate {predicate} rendered as {term}: {error}"
            );
        }
    }

    /// The existence and property-in-set forms read the same rejection —
    /// the sentence names each term's own shape.
    #[test]
    fn the_existence_and_property_in_set_forms_read_the_same_rejection() {
        for predicate in ["movie.rating IS NOT NULL", "movie.title IN ['a']"] {
            let error = fulltext(predicate).expect_err("a property predicate must be rejected");
            assert!(
                error.to_string().contains(&format!(
                    "The search filter predicate '{predicate}' is not supported"
                )),
                "{error}"
            );
        }
    }

    /// A wrong variable on a *legal* form keeps the entity-set rule's own
    /// rejection — the kind gate never preempts the more specific error.
    #[test]
    fn a_wrong_variable_on_a_legal_form_keeps_its_own_rejection() {
        let error =
            fulltext("other IN $candidates").expect_err("a non-binding variable must be rejected");
        assert_eq!(
            error.to_string(),
            "syntax error: WrongVariableForVectorSearchFilter",
            "the entity-set rule's rejection is the one to raise"
        );
    }
}

/// The combination rejection is kind-aware (ADR-0714): two match filters
/// on a fulltext window read the reference server's invalid-predicate family, not
/// the vector combination sentence — the sentence never names the wrong
/// index kind.
mod fulltext_combination {
    use super::fulltext_grammar::fulltext;

    #[test]
    fn two_match_filters_read_the_fulltext_family() {
        let error = fulltext("movie IN $a AND movie IN $b")
            .expect_err("the match filter appears at most once");
        assert!(
            error.to_string().contains(
                "invalid predicate for search with filters. The search filter \
                 predicate combination is not supported"
            ),
            "{error}"
        );
        assert!(
            !error.to_string().contains("vector search filtering"),
            "a fulltext path never reads the vector sentence: {error}"
        );
    }
}
