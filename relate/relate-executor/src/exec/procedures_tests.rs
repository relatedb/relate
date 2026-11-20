//! The `CALL` procedure contract's behavior tests (the executor's
//! `procedures` module family): the argument type vocabulary's fail-closed
//! edge, the diverted scan contract's paren-less rejections, the YIELD
//! WHERE typed gate, and the row-provenance shapes of the registry path.

mod type_vocabulary_tests {
    use crate::procedure_registry::value_matches_proc_type;
    use relate_core::Value;

    /// ADR-0463 round 6: the list spelling is exactly `LIST` or `LIST<…>`;
    /// a bare prefix match would silently accept `LISTS`/`LISTING` as any
    /// list, re-opening the fail-closed vocabulary.
    #[test]
    fn list_type_accepts_only_list_or_list_generic() {
        let list = Value::List(Vec::new().into());
        assert!(value_matches_proc_type(&list, "LIST"));
        assert!(value_matches_proc_type(&list, "LIST<FLOAT>"));
        assert!(value_matches_proc_type(&list, "list<string>"));
        assert!(!value_matches_proc_type(&list, "LISTS"));
        assert!(!value_matches_proc_type(&list, "LISTING"));
        assert!(!value_matches_proc_type(&Value::Int(1), "LISTS"));
    }
}

mod row_provenance_tests {
    use std::collections::BTreeMap;

    use crate::exec::test_support::{
        exec_autocommit, exec_cypher, try_exec_cypher, try_exec_cypher_params,
    };
    use crate::graph::Graph;
    use relate_core::Value;

    /// The diverted scan calls read their arguments positionally, so the
    /// one contract check every dispatch path reads (stellar round 2,
    /// F-2-26/F-2-43) states the paren-less form's rejection by its
    /// form: with a YIELD it is the passing-mode violation — never a
    /// misleading arity count — and standalone it is the arity
    /// rejection over the empty list the grammar passes.
    #[test]
    fn the_diverted_scan_contract_rejects_paren_less_calls_by_their_form() {
        let graph = Graph::default();
        let in_query = try_exec_cypher(&graph, "CALL db.index.vector.queryNodes YIELD node")
            .err()
            .expect("an in-query paren-less vector call is the passing-mode rejection");
        assert!(
            in_query.to_string().contains("InvalidArgumentPassingMode"),
            "{in_query}"
        );
        let standalone = try_exec_cypher(&graph, "CALL db.index.vector.queryNodes")
            .err()
            .expect("a paren-less vector call passes no positional list");
        let text = standalone.to_string();
        assert!(
            text.contains("InvalidNumberOfArguments")
                && text.contains("db.index.vector.queryNodes expected 3, got 0"),
            "{text}"
        );
    }

    /// A `CALL`'s `YIELD … WHERE` is a predicate position: a runtime
    /// non-boolean (a parameter bound to an integer) is the typed
    /// `requires a boolean` rejection every other optional-WHERE site
    /// raises through the one `where_clause_holds` gate — never a silent
    /// row drop (the composer once hand-spelled the truthy check).
    #[test]
    fn a_call_yield_where_with_a_non_boolean_parameter_is_the_typed_rejection() {
        let graph = Graph::default();
        exec_autocommit(&graph, "CREATE (:User), (:Admin)").expect("the fixture's CREATE succeeds");
        let params: BTreeMap<String, Value> =
            [("flag".to_string(), Value::Int(7))].into_iter().collect();
        let error = try_exec_cypher_params(
            &graph,
            "CALL db.labels() YIELD label WHERE $flag RETURN label",
            &params,
        )
        .err()
        .expect("a non-boolean CALL YIELD WHERE predicate must be rejected");
        let text = error.to_string();
        assert!(
            text.contains("InvalidArgumentType") && text.contains("requires a boolean"),
            "{text}"
        );
    }

    /// A no-argument built-in's rows are produced once for the call and
    /// answered by every driving row — two driving rows times two labels,
    /// never one production that only the first row sees.
    #[test]
    fn a_no_argument_builtin_answers_its_rows_for_every_driving_row() {
        let graph = Graph::default();
        exec_autocommit(&graph, "CREATE (:User), (:Admin)").expect("the fixture's CREATE succeeds");

        let result = exec_cypher(
            &graph,
            "UNWIND [1, 2] AS i CALL db.labels() YIELD label \
             RETURN i, label ORDER BY i, label",
        );
        let string = |value: &str| Value::String(value.into());
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Int(1), string("Admin")],
                vec![Value::Int(1), string("User")],
                vec![Value::Int(2), string("Admin")],
                vec![Value::Int(2), string("User")],
            ]
        );
    }

    /// A built-in whose producer reads its arguments produces them under
    /// every driving row's own binding: `db.stats.retrieve` is served the
    /// section each row carries, never an empty argument map.
    #[test]
    fn an_argument_taking_builtin_produces_rows_from_each_rows_bindings() {
        let graph = Graph::default();

        let result = exec_cypher(
            &graph,
            "UNWIND [1, 2] AS i WITH i, 'GRAPH COUNTS' AS wanted \
             CALL db.stats.retrieve(wanted) YIELD section RETURN i, section",
        );
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Int(1), Value::String("GRAPH COUNTS".into())],
                vec![Value::Int(2), Value::String("GRAPH COUNTS".into())],
            ]
        );
    }
}

mod producer_table_pin {
    use std::collections::BTreeMap;

    use crate::exec::procedure_rows::builtin_procedure_rows;
    use crate::graph::Graph;
    use crate::overlay::ExecGraph;
    use crate::procedure_registry::BUILTIN_PROCEDURE_SIGNATURES;
    use relate_core::Value;

    /// The producer table and the registry agree: every executable signature
    /// that declares return columns either has a diverted call path or a
    /// producer arm in its registry row — asserted as the table fact
    /// itself, never by classifying the probe's error text. The producer
    /// call stays as smoke: every declared parameter bound to a
    /// representative value, so a producer that reads its arguments is
    /// exercised rather than panicking on the CALL contract's missing
    /// binding. (Lives here, beside the procedures module it reads, so the
    /// lower-level `procedure_rows` never reaches up into its caller, even
    /// in tests.)
    #[test]
    fn every_row_declaring_signature_has_a_producer_or_a_diverted_path() {
        let graph = Graph::default();
        let view = ExecGraph::read(&graph);
        for signature in BUILTIN_PROCEDURE_SIGNATURES {
            // The canonical void predicate, never a re-derivation from
            // `returns` — `is_void` is documented as the one fact.
            if !signature.executable || signature.is_void() {
                continue;
            }
            if crate::exec::procedures::divert_call(&relate_names::SmallName::from_name(
                signature.name,
            ))
            .is_some()
            {
                continue;
            }
            assert!(
                signature.rows.is_some(),
                "{} declares return columns but the registry row carries no producer",
                signature.name
            );
            let args: BTreeMap<String, Value> = signature
                .args
                .iter()
                .map(|arg| (arg.name.to_string(), Value::String("GRAPH COUNTS".into())))
                .collect();
            let _ = builtin_procedure_rows(&view, signature, None, &args);
        }
    }
}
