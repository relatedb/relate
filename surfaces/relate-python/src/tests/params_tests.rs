//! Parameter conversion (Python -> Cypher): rich temporal/point params,
//! cycles and depth caps, UUID/bytes/tuple params, ordered maps, DB-API errors.

use super::*;

#[test]
fn test_datetime_param_naive_is_localdatetime() {
    with_py(|py| {
        let dtmod = py.import("datetime").unwrap();
        let dt = dtmod
            .call_method1("datetime", (2024, 3, 14, 12, 30, 45))
            .unwrap();
        let cv = py_to_cypher_value(&dt).unwrap();
        assert!(matches!(cv, CypherValue::LocalDateTime(_)), "{cv:?}");
    });
}

#[test]
fn test_date_param_roundtrips() {
    with_py(|py| {
        let dtmod = py.import("datetime").unwrap();
        let d = dtmod.call_method1("date", (2024, 3, 14)).unwrap();
        let cv = py_to_cypher_value(&d).unwrap();
        match cv {
            CypherValue::Date(nd) => {
                let back = date_to_py(py, &nd).unwrap();
                assert!(back.bind(py).eq(&d).unwrap());
            }
            other => panic!("expected Date, got {other:?}"),
        }
    });
}

#[test]
fn test_point_param_roundtrips() {
    with_py(|py| {
        let pt = Py::new(
            py,
            Point {
                srid: 4326,
                x: -73.98,
                y: 40.75,
                z: None,
                crs: "wgs-84".to_string(),
            },
        )
        .unwrap();
        let bound = pt.bind(py).clone().into_any();
        let cv = py_to_cypher_value(&bound).unwrap();
        match cv {
            CypherValue::Point(p) => {
                assert_eq!(p.srid, 4326);
                assert_eq!(p.x, -73.98);
                assert_eq!(p.y, 40.75);
            }
            other => panic!("expected Point, got {other:?}"),
        }
    });
}

#[test]
fn test_unknown_param_type_raises() {
    with_py(|py| {
        // A set is not convertible.
        let obj = py.eval(c"{1, 2, 3}", None, None).unwrap();
        let err = py_to_cypher_value(&obj).unwrap_err();
        assert!(err.is_instance_of::<PyTypeError>(py));
    });
}

#[test]
fn test_cyclic_list_param_raises() {
    with_py(|py| {
        // a = []; a.append(a)
        let list = PyList::empty(py);
        list.append(list.clone()).unwrap();
        let obj = list.into_any();
        let err = py_to_cypher_value(&obj).unwrap_err();
        assert!(err.is_instance_of::<PyValueError>(py));
        assert!(
            err.to_string().to_lowercase().contains("cycle"),
            "expected cycle in message, got: {err}"
        );
    });
}

#[test]
fn test_cyclic_dict_param_raises() {
    with_py(|py| {
        // d = {}; d["self"] = d
        let dict = PyDict::new(py);
        dict.set_item("self", dict.clone()).unwrap();
        let obj = dict.into_any();
        let err = py_to_cypher_value(&obj).unwrap_err();
        assert!(err.is_instance_of::<PyValueError>(py));
        assert!(
            err.to_string().to_lowercase().contains("cycle"),
            "expected cycle in message, got: {err}"
        );
    });
}

#[test]
fn test_over_deep_nesting_raises() {
    with_py(|py| {
        // Build far more than `PY_VALUE_DEPTH_CAP` nested single-item lists.
        let mut current = PyList::empty(py).into_any();
        for _ in 0..(PY_VALUE_DEPTH_CAP + 64) {
            let outer = PyList::empty(py);
            outer.append(current.clone()).unwrap();
            current = outer.into_any();
        }
        let err = py_to_cypher_value(&current).unwrap_err();
        assert!(err.is_instance_of::<PyValueError>(py));
        assert!(
            err.to_string().contains("depth"),
            "expected depth in message, got: {err}"
        );
    });
}

#[test]
fn test_shared_substructure_is_not_a_cycle() {
    with_py(|py| {
        // The same list referenced twice is a DAG, not a cycle: it must
        // still convert, since the shared node is only on the path once.
        let shared = PyList::new(py, [1, 2, 3]).unwrap();
        let outer = PyList::empty(py);
        outer.append(shared.clone()).unwrap();
        outer.append(shared.clone()).unwrap();
        let obj = outer.into_any();
        assert_eq!(expect_list(py_to_cypher_value(&obj).unwrap()).len(), 2);
    });
}

#[test]
fn test_geometry_is_structured_dict() {
    with_py(|py| {
        let mut db = new_database(":memory:");
        let bound = query_first_py(py, &mut db, "RETURN point({x: 1.0, y: 2.0}) AS p", "p");
        // A point becomes a Point instance, not a dict.
        assert!(bound.is_instance_of::<Point>());
    });
}

#[test]
fn temporal_and_point_parameters_remain_rich() {
    with_py(|py| {
        let dtmod = py.import("datetime").unwrap();
        let date = dtmod.call_method1("date", (2024, 3, 14)).unwrap();
        assert!(matches!(
            py_to_cypher_value(&date).unwrap(),
            CypherValue::Date(_)
        ));

        let point = Py::new(py, Point::new(4326, -73.98, 40.75, None)).unwrap();
        assert!(matches!(
            py_to_cypher_value(&point.into_bound(py).into_any()).unwrap(),
            CypherValue::Point(_)
        ));
    });
}

// ============================================================
// ADR-0219: UUID / bytes / tuple params, explicit-only points,
// ordered maps, DB-API-worded parameter errors
// ============================================================

#[test]
fn test_uuid_param_becomes_canonical_string() {
    with_py(|py| {
        let uuid_mod = py.import("uuid").unwrap();
        let u = uuid_mod
            .call_method1("UUID", ("12345678-1234-5678-9abc-def012345678",))
            .unwrap();
        let cv = py_to_cypher_value(&u).unwrap();
        assert_eq!(expect_str(cv), "12345678-1234-5678-9abc-def012345678");
        // Uppercase input normalizes to the canonical lowercase form.
        let u2 = uuid_mod
            .call_method1("UUID", ("12345678-1234-5678-9ABC-DEF012345678",))
            .unwrap();
        assert_eq!(
            expect_str(py_to_cypher_value(&u2).unwrap()),
            "12345678-1234-5678-9abc-def012345678"
        );
    });
}

#[test]
fn test_bytes_param_becomes_int_list() {
    with_py(|py| {
        let obj = PyBytes::new(py, b"\x00\x01\xff").as_any().clone();
        assert_eq!(
            expect_list(py_to_cypher_value(&obj).unwrap()),
            vec![
                CypherValue::Int(0),
                CypherValue::Int(1),
                CypherValue::Int(255)
            ]
        );
        // An empty payload converts too.
        let empty = PyBytes::new(py, b"").as_any().clone();
        assert!(expect_list(py_to_cypher_value(&empty).unwrap()).is_empty());
    });
}

#[test]
fn test_bytearray_param_becomes_int_list() {
    with_py(|py| {
        let obj = py.eval(c"bytearray(b'AB')", None, None).unwrap();
        assert_eq!(
            expect_list(py_to_cypher_value(&obj).unwrap()),
            vec![CypherValue::Int(65), CypherValue::Int(66)]
        );
    });
}

#[test]
fn test_tuple_param_becomes_list() {
    with_py(|py| {
        let obj = py.eval(c"(1, 'two', 3.5, (4, 5))", None, None).unwrap();
        let mut items = expect_list(py_to_cypher_value(&obj).unwrap());
        assert_eq!(items[0], CypherValue::Int(1));
        assert_eq!(items[1], CypherValue::Str("two".to_string()));
        assert_eq!(items[2], CypherValue::Float(3.5));
        let inner = expect_list(items.swap_remove(3));
        assert_eq!(inner, vec![CypherValue::Int(4), CypherValue::Int(5)]);
    });
}

#[test]
fn test_cyclic_tuple_param_raises() {
    with_py(|py| {
        // t = ([],); t[0].append(t)
        let obj = py
            .eval(c"__import__('builtins').tuple([[]])", None, None)
            .unwrap();
        obj.get_item(0)
            .unwrap()
            .call_method1("append", (&obj,))
            .unwrap();
        let err = py_to_cypher_value(&obj).unwrap_err();
        assert!(err.is_instance_of::<PyValueError>(py));
    });
}

#[test]
fn test_srid_dict_stays_a_map() {
    with_py(|py| {
        for text in [
            c"{'srid': 4326, 'x': 1.0, 'y': 2.0}",
            c"{'srid': 4326, 'longitude': -73.98, 'latitude': 40.75}",
            c"{'srid': 9157, 'x': 1.0, 'y': 2.0, 'z': 3.0}",
        ] {
            let obj = py.eval(text, None, None).unwrap();
            let map = expect_map(py_to_cypher_value(&obj).unwrap());
            assert!(map.contains_key("srid"), "map lost its srid key");
        }
    });
}

#[test]
fn test_returned_map_preserves_engine_key_order() {
    with_py(|py| {
        // Keys deliberately not alphabetical: the Python dict must keep
        // the engine IndexMap's insertion order (ADR-0219).
        let mut map = IndexMap::new();
        map.insert("zeta".to_string(), CypherValue::Int(1));
        map.insert("alpha".to_string(), CypherValue::Int(2));
        map.insert(
            "mid".to_string(),
            CypherValue::Map(IndexMap::from_iter([
                ("y".to_string(), CypherValue::Int(3)),
                ("b".to_string(), CypherValue::Int(4)),
            ])),
        );
        let obj = cypher_value_to_py(py, &CypherValue::Map(map), Namespace::Neo4j).unwrap();
        let dict = obj.into_bound(py);
        let keys: Vec<String> = dict
            .downcast::<PyDict>()
            .unwrap()
            .keys()
            .iter()
            .map(|k| k.extract().unwrap())
            .collect();
        assert_eq!(keys, vec!["zeta", "alpha", "mid"]);
        let nested = dict.get_item("mid").unwrap();
        let nested_keys: Vec<String> = nested
            .downcast::<PyDict>()
            .unwrap()
            .keys()
            .iter()
            .map(|k| k.extract().unwrap())
            .collect();
        assert_eq!(nested_keys, vec!["y", "b"]);
    });
}

#[test]
fn test_non_dict_params_error_is_dbapi_worded() {
    with_py(|py| {
        let list = PyList::empty(py).into_any();
        let err = py_params_to_cypher(&list).unwrap_err();
        assert!(err.is_instance_of::<PyTypeError>(py));
        let msg = err.to_string();
        assert!(
            msg.contains("parameters must be a dict"),
            "unexpected message: {msg}"
        );
        assert!(msg.contains("'list'"), "message must name the type: {msg}");
    });
}

#[test]
fn test_non_string_param_key_error_is_dbapi_worded() {
    with_py(|py| {
        let dict = PyDict::new(py);
        dict.set_item(42, 1).unwrap();
        let err = py_params_to_cypher(dict.as_any()).unwrap_err();
        assert!(err.is_instance_of::<PyTypeError>(py));
        let msg = err.to_string();
        assert!(
            msg.contains("keys must be strings"),
            "unexpected message: {msg}"
        );
        assert!(msg.contains("'int'"), "message must name the type: {msg}");

        // Same wording inside a nested map parameter value.
        let inner = PyDict::new(py);
        inner.set_item(1.5, "v").unwrap();
        let err = py_to_cypher_value(inner.as_any()).unwrap_err();
        assert!(err.is_instance_of::<PyTypeError>(py));
        assert!(
            err.to_string().contains("keys must be strings"),
            "unexpected message: {err}"
        );
    });
}
