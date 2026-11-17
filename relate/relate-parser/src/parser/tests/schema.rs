use super::*;
use crate::parser::schema::{is_command_name_token, COMMAND_NAME_CONTINUATIONS};
use crate::token::keyword_spellings;
use crate::YieldMode;

#[test]
fn test_parse_create_index() {
    let stmt = parse("CREATE INDEX myidx FOR (n:Person) ON (n.name)").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateIndex(idx) => {
            assert_eq!(idx.name, Some(SchemaName::Literal("myidx".into())));
            assert_eq!(idx.labels, vec!["Person"]);
            assert_eq!(idx.properties, vec!["name"]);
            assert_eq!(idx.entity_type, SchemaEntityType::Node);
            assert_eq!(idx.kind, IndexKind::Range);
        }
        _ => panic!("expected create index"),
    }
}

#[test]
fn test_parse_drop_index() {
    let stmt = parse("DROP INDEX myidx IF EXISTS").unwrap();
    match &stmt.clauses[0] {
        Clause::DropIndex(d) => {
            assert_eq!(d.name, SchemaName::Literal("myidx".into()));
            assert!(d.if_exists);
        }
        _ => panic!("expected drop index"),
    }
}

// `IF EXISTS` may precede or follow the DROP INDEX/CONSTRAINT name, once.
#[test]
fn test_parse_drop_if_exists_positions() {
    for (q, want_if_exists) in [
        ("DROP INDEX x", false),
        ("DROP INDEX IF EXISTS x", true),
        ("DROP INDEX x IF EXISTS", true),
        ("DROP CONSTRAINT x", false),
        ("DROP CONSTRAINT IF EXISTS x", true),
        ("DROP CONSTRAINT x IF EXISTS", true),
    ] {
        let stmt = parse(q).unwrap();
        let (name, if_exists) = match &stmt.clauses[0] {
            Clause::DropIndex(d) => (&d.name, d.if_exists),
            Clause::DropConstraint(d) => (&d.name, d.if_exists),
            other => panic!("{q}: expected a drop clause, got {other:?}"),
        };
        assert_eq!(name, &SchemaName::Literal("x".into()), "{q}");
        assert_eq!(if_exists, want_if_exists, "{q}");
    }
    assert!(parse("DROP INDEX IF EXISTS x IF EXISTS").is_err());
    assert!(parse("DROP CONSTRAINT IF EXISTS x IF EXISTS").is_err());
}

// Legacy `ON :Label(prop[, ...])` index targets: CREATE INDEX takes a
// property list, DROP INDEX exactly one property.
#[test]
fn test_parse_legacy_index_targets() {
    let stmt = parse("CREATE INDEX ON :L(a, b)").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateIndex(idx) => {
            assert_eq!(idx.labels, vec!["L"]);
            assert_eq!(idx.properties, vec!["a", "b"]);
        }
        _ => panic!("expected create index"),
    }
    let stmt = parse("DROP INDEX ON :L(a)").unwrap();
    match &stmt.clauses[0] {
        Clause::DropIndex(d) => {
            assert_eq!(d.legacy_target, Some(("L".into(), "a".into())));
        }
        _ => panic!("expected drop index"),
    }
    let err = parse("DROP INDEX ON :L(a, b)").unwrap_err();
    assert_eq!(err.detail, None, "{}", err.message);
    assert_eq!(
        err.message,
        "legacy DROP INDEX ON :Label(prop) takes exactly one property"
    );
    assert_eq!((err.line, err.col), (1, 12), "{err}");
    let err = parse("CREATE INDEX ON :L(a, 1)").unwrap_err();
    assert!(
        err.message.starts_with("expected property name"),
        "{}",
        err.message
    );
}

#[test]
fn test_parse_create_constraint() {
    let stmt = parse("CREATE CONSTRAINT mycon FOR (n:Person) REQUIRE n.name IS UNIQUE").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateConstraint(c) => {
            assert_eq!(c.name, Some(SchemaName::Literal("mycon".into())));
            assert_eq!(c.label, "Person");
            assert_eq!(c.constraint_type, "unique");
        }
        _ => panic!("expected create constraint"),
    }
}

// The constraint's property name goes through the symbolic-name reader: a
// token that is neither identifier nor keyword is a parse error, never a
// property literally named by that token.
#[test]
fn test_parse_create_constraint_rejects_non_identifier_property_name() {
    // `.123` lexes as one float token, so the dot itself already fails.
    assert!(parse("CREATE CONSTRAINT c FOR (n:L) REQUIRE n.123").is_err());
    // A parameter token after the dot reaches the symbolic-name reader.
    let err = parse("CREATE CONSTRAINT c FOR (n:L) REQUIRE n.$p").unwrap_err();
    assert!(
        err.message.starts_with("expected property name"),
        "{}",
        err.message
    );
}

// 42N62: a constraint property over any variable other than the `FOR`
// clause's own keeps the UndefinedVariable detail, like every sibling site.
#[test]
fn test_parse_create_constraint_undefined_variable_keeps_detail() {
    let err = parse("CREATE CONSTRAINT c FOR (n:L) REQUIRE x.name IS KEY").unwrap_err();
    assert_eq!(
        err.detail,
        Some(crate::parser::UNDEFINED_VARIABLE_DETAIL),
        "{}",
        err.message
    );
}

// CIP-134: $param in the name position of CREATE/DROP INDEX/CONSTRAINT.
#[test]
fn test_parse_create_index_param_name() {
    let stmt = parse("CREATE INDEX $param FOR (n:Person) ON (n.name)").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateIndex(idx) => {
            assert_eq!(idx.name, Some(SchemaName::Parameter("param".into())));
        }
        _ => panic!("expected create index"),
    }
}

#[test]
fn test_parse_drop_index_param_name() {
    let stmt = parse("DROP INDEX $param IF EXISTS").unwrap();
    match &stmt.clauses[0] {
        Clause::DropIndex(d) => {
            assert_eq!(d.name, SchemaName::Parameter("param".into()));
            assert!(d.if_exists);
        }
        _ => panic!("expected drop index"),
    }
}

#[test]
fn test_parse_create_constraint_param_name() {
    let stmt = parse("CREATE CONSTRAINT $param FOR (n:Person) REQUIRE n.name IS UNIQUE").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateConstraint(c) => {
            assert_eq!(c.name, Some(SchemaName::Parameter("param".into())));
        }
        _ => panic!("expected create constraint"),
    }
}

#[test]
fn test_parse_drop_constraint_param_name() {
    let stmt = parse("DROP CONSTRAINT $param IF EXISTS").unwrap();
    match &stmt.clauses[0] {
        Clause::DropConstraint(d) => {
            assert_eq!(d.name, SchemaName::Parameter("param".into()));
            assert!(d.if_exists);
        }
        _ => panic!("expected drop constraint"),
    }
}

// CIP-137/CIP-11: a keyword like INDEX or CONSTRAINT can itself be used
// unescaped as the name of an index or constraint.
#[test]
fn test_parse_create_constraint_keyword_name() {
    let stmt =
        parse("CREATE CONSTRAINT constraint FOR (n:Label) REQUIRE (n.property) IS KEY").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateConstraint(c) => {
            assert_eq!(c.name, Some(SchemaName::Literal("constraint".into())));
        }
        _ => panic!("expected create constraint"),
    }
}

#[test]
fn test_parse_create_index_keyword_name() {
    let stmt = parse("CREATE INDEX index FOR (n:Label) ON (n.property)").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateIndex(idx) => {
            assert_eq!(idx.name, Some(SchemaName::Literal("index".into())));
        }
        _ => panic!("expected create index"),
    }
}

// LABS-1614: the NULL keyword names a schema object literally called
// "null" — it is not an unnamed form. Neo4j 2026.10's commandNameExpression
// admits every keyword unquoted in the name position; NULL is the reported
// spelling (the LABS-1600 EXPLAIN probe), and the derived predicate
// (`is_command_name_token`) admits every keyword outside the commands'
// name-slot continuations — TRUE, pinned in the table below through the
// same IF NOT EXISTS shape, is one more such spelling.
#[test]
fn test_parse_create_constraint_null_name() {
    // The exact reported repro: the name before IF NOT EXISTS.
    let stmt =
        parse("CREATE CONSTRAINT null IF NOT EXISTS FOR (n:L1) REQUIRE n.p1 IS UNIQUE").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateConstraint(c) => {
            assert_eq!(c.name, Some(SchemaName::Literal("null".into())));
            assert!(c.if_not_exists);
        }
        _ => panic!("expected create constraint"),
    }
    // The plain form, the name after IF NOT EXISTS, the uppercase
    // spelling (the name keeps the case as written), and a second
    // newly-admitted keyword spelling through the repro's own shape.
    for (q, want_name, if_not_exists) in [
        (
            "CREATE CONSTRAINT null FOR (n:L) REQUIRE n.p IS UNIQUE",
            "null",
            false,
        ),
        (
            "CREATE CONSTRAINT IF NOT EXISTS null FOR (n:L) REQUIRE n.p IS UNIQUE",
            "null",
            true,
        ),
        (
            "CREATE CONSTRAINT NULL FOR (n:L) REQUIRE n.p IS UNIQUE",
            "NULL",
            false,
        ),
        (
            "CREATE CONSTRAINT true IF NOT EXISTS FOR (n:L1) REQUIRE n.p1 IS UNIQUE",
            "true",
            true,
        ),
    ] {
        let stmt = parse(q).unwrap();
        match &stmt.clauses[0] {
            Clause::CreateConstraint(c) => {
                assert_eq!(c.name, Some(SchemaName::Literal(want_name.into())), "{q}");
                assert_eq!(c.if_not_exists, if_not_exists, "{q}");
            }
            _ => panic!("{q}: expected create constraint"),
        }
    }
}

// The same one-token fix covers the sibling name positions the shared
// reader feeds: CREATE INDEX and both DROP readers. A name creatable in
// one position must be droppable in the other, so DROP CONSTRAINT null
// and DROP INDEX null pin the required-name side.
#[test]
fn test_parse_null_name_in_sibling_positions() {
    let stmt = parse("CREATE INDEX null FOR (n:Label) ON (n.property)").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateIndex(idx) => {
            assert_eq!(idx.name, Some(SchemaName::Literal("null".into())));
        }
        _ => panic!("expected create index"),
    }
    for q in [
        "DROP CONSTRAINT null",
        "DROP CONSTRAINT null IF EXISTS",
        "DROP CONSTRAINT IF EXISTS null",
        "DROP INDEX null",
    ] {
        let stmt = parse(q).unwrap();
        match &stmt.clauses[0] {
            Clause::DropConstraint(d) => {
                assert_eq!(d.name, SchemaName::Literal("null".into()), "{q}");
            }
            Clause::DropIndex(d) => {
                assert_eq!(d.name, SchemaName::Literal("null".into()), "{q}");
            }
            other => panic!("{q}: expected a drop clause, got {other:?}"),
        }
    }
}

// Genuinely invalid name spellings stay rejected: an integer token, a
// string literal (Neo4j's commandNameExpression has no string arm), and a
// keyword that would collide with the name-position continuation (`IF`
// would misparse `CREATE CONSTRAINT IF NOT EXISTS ...` as naming the
// constraint "IF").
#[test]
fn test_parse_create_constraint_rejects_non_name_tokens() {
    assert!(parse("CREATE CONSTRAINT 123 FOR (n:L) REQUIRE n.p IS UNIQUE").is_err());
    assert!(parse("CREATE CONSTRAINT \"null\" FOR (n:L) REQUIRE n.p IS UNIQUE").is_err());
    assert!(parse("CREATE CONSTRAINT if FOR (n:L) REQUIRE n.p IS UNIQUE").is_err());
}

/// LABS-1614: the command-name slot's admissible set, pinned as the one
/// predicate the schema-name readers consult (`schema.rs`, beside
/// `parse_optional_schema_name`). The predicate is layered --
/// `is_schema_name() && !COMMAND_NAME_CONTINUATIONS` -- so a
/// spelling-by-spelling re-assertion of that formula would be tautological;
/// each half is pinned through its own named members instead: every
/// continuation token stays rejected, its command (the const entry's
/// second element, the one home of that fact) named in the failure
/// message, the documented admissible keyword samples stay admitted, and
/// the non-name token classes stay rejected. A token dropped from the
/// closed set is caught in position, by
/// `test_command_name_continuations_in_position` below.
#[test]
fn test_is_command_name_token() {
    assert!(is_command_name_token(TokenType::Identifier));
    // The closed set holds only keyword spellings of the one vocabulary
    // declaration (read through the canonical iterator beside it) — the
    // contract is "the commands' keyword tail starters", so a member no
    // lexer can produce is dead weight this catches.
    for (ty, _) in COMMAND_NAME_CONTINUATIONS {
        assert!(
            keyword_spellings().any(|(_, spelling_ty)| spelling_ty == ty),
            "{ty:?} is not a keyword spelling"
        );
    }
    // Every continuation token stays rejected, the command that reads it
    // (the const's own second element) named in the failure message.
    for (ty, command) in COMMAND_NAME_CONTINUATIONS {
        assert!(!is_command_name_token(ty), "{ty:?} continues {command}");
    }
    // The admissible keyword samples: the bare schema keywords and the
    // LABS-1614 repro spelling, plus two keywords that sit inside the
    // commands' qualifiers but never at the name slot — TRUE and EXISTS —
    // which the full-grammar derivation newly admits.
    for ty in [
        TokenType::INDEX,
        TokenType::CONSTRAINT,
        TokenType::NULL,
        TokenType::TRUE,
        TokenType::EXISTS,
    ] {
        assert!(is_command_name_token(ty), "{ty:?} must be a valid name");
    }
    // The non-name token classes: the predicate is only the
    // keyword/identifier half of the slot — the reader's own Parameter
    // arm admits the `$param` reference (CIP-134).
    for ty in [TokenType::Parameter, TokenType::String, TokenType::Integer] {
        assert!(!is_command_name_token(ty), "{ty:?}");
    }
}

/// The continuations stay continuations in position — each command's
/// unnamed, qualified, legacy-target, action and tail forms keep parsing
/// with no keyword swallowed as a name — and the widened keyword
/// spellings (EXISTS here; TRUE in the constraint-name table above) parse
/// as names in the same slots.
#[test]
fn test_command_name_continuations_in_position() {
    // CREATE INDEX/CONSTRAINT: the IF NOT EXISTS qualifier, the legacy
    // ON target and the FOR target are all name-less spellings.
    for q in [
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:L) REQUIRE n.p IS UNIQUE",
        "CREATE INDEX ON :L(a)",
        "CREATE CONSTRAINT FOR (n:L) REQUIRE n.p IS UNIQUE",
    ] {
        let stmt = parse(q).unwrap();
        let name = match &stmt.clauses[0] {
            Clause::CreateConstraint(c) => &c.name,
            Clause::CreateIndex(i) => &i.name,
            other => panic!("{q}: expected a create clause, got {other:?}"),
        };
        assert_eq!(name, &None, "{q}: a continuation was swallowed as the name");
    }
    // IF always continues the command, so a constraint actually named
    // `if` stays unreachable.
    assert!(parse("CREATE CONSTRAINT if FOR (n:L) REQUIRE n.p IS UNIQUE").is_err());
    // ALTER DATABASE: the name is required, and SET is the action after
    // it — a missing name fails as a missing name, not by naming the
    // database "set".
    let stmt = parse("ALTER DATABASE movies IF EXISTS SET DEFAULT LANGUAGE CYPHER 25").unwrap();
    let Clause::AlterDatabase(alter) = &stmt.clauses[0] else {
        panic!("expected AlterDatabase, got {:?}", stmt.clauses[0]);
    };
    assert_eq!(alter.name, SchemaName::Literal("movies".into()));
    assert!(alter.if_exists);
    assert_eq!(alter.default_language, 25);
    assert!(parse("ALTER DATABASE set SET DEFAULT LANGUAGE CYPHER 25").is_err());
    // SHOW DATABASE: the YIELD projection, the paging stages and the
    // WHERE tail all continue the unnamed form.
    for (q, want_stages, want_where) in [
        ("SHOW DATABASES YIELD name", 0, false),
        ("SHOW DATABASES ORDER BY name SKIP 1 LIMIT 2", 3, false),
        ("SHOW DATABASES WHERE name = 'neo4j'", 0, true),
    ] {
        let stmt = parse(q).unwrap();
        let Clause::ShowDatabases(show) = &stmt.clauses[0] else {
            panic!("{q}: expected ShowDatabases, got {:?}", stmt.clauses[0]);
        };
        assert_eq!(
            show.filter,
            DatabaseFilter::All,
            "{q}: a continuation was swallowed as the name"
        );
        assert_eq!(show.stages.len(), want_stages, "{q}");
        assert_eq!(show.where_expr.is_some(), want_where, "{q}");
    }
    // The widened EXISTS spelling: an index named "exists", followed by
    // its own IF EXISTS qualifier.
    let stmt = parse("DROP INDEX exists IF EXISTS").unwrap();
    let Clause::DropIndex(drop) = &stmt.clauses[0] else {
        panic!("expected DropIndex, got {:?}", stmt.clauses[0]);
    };
    assert_eq!(drop.name, SchemaName::Literal("exists".into()));
    assert!(drop.if_exists);
}

// `$$param` (double-dollar) is not recognized
// parameter syntax and must be rejected as a parse error.
#[test]
fn test_parse_create_index_double_dollar_name_rejected() {
    assert!(parse("CREATE INDEX $$param FOR (n:Label) ON (n.property)").is_err());
}

#[test]
fn test_parse_drop_index_double_dollar_name_rejected() {
    assert!(parse("DROP INDEX $$param").is_err());
}

#[test]
fn test_parse_create_constraint_double_dollar_name_rejected() {
    assert!(
        parse("CREATE CONSTRAINT $$param FOR (n:Label) REQUIRE (n.property) IS NODE KEY").is_err()
    );
}

/// CIP-62: every `IS [entity] (UNIQUE|KEY)` keyword-surface spelling
/// parses to the plain `"unique"`/`"key"` constraint_type -- the entity
/// keyword itself (NODE/REL/RELATIONSHIP) is documentary only, since
/// the real entity comes from the FOR-clause target.
#[test]
fn test_parse_create_constraint_entity_keyword_surface() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "CREATE CONSTRAINT c FOR (n:P) REQUIRE n.x IS NODE UNIQUE",
            "unique",
            "NODE",
        ),
        (
            "CREATE CONSTRAINT c FOR ()-[r:T]-() REQUIRE r.x IS RELATIONSHIP UNIQUE",
            "unique",
            "RELATIONSHIP",
        ),
        (
            "CREATE CONSTRAINT c FOR ()-[r:T]-() REQUIRE r.x IS REL UNIQUE",
            "unique",
            "RELATIONSHIP",
        ),
        (
            "CREATE CONSTRAINT c FOR ()-[r:T]-() REQUIRE r.x IS RELATIONSHIP KEY",
            "key",
            "RELATIONSHIP",
        ),
        (
            "CREATE CONSTRAINT c FOR ()-[r:T]-() REQUIRE r.x IS REL KEY",
            "key",
            "RELATIONSHIP",
        ),
        (
            "CREATE CONSTRAINT c FOR ()-[r:T]-() REQUIRE r.x IS KEY",
            "key",
            "RELATIONSHIP",
        ),
        (
            "CREATE CONSTRAINT c FOR (n:P) REQUIRE n.x IS KEY",
            "key",
            "NODE",
        ),
    ];
    for (query, want_type, want_entity) in cases {
        let stmt = parse(query).unwrap();
        match &stmt.clauses[0] {
            Clause::CreateConstraint(c) => {
                assert_eq!(c.constraint_type, *want_type, "{query}");
                assert_eq!(c.entity_type.as_str(), *want_entity, "{query}");
            }
            _ => panic!("expected create constraint for {query}"),
        }
    }
}

/// CIP-62/CIP-64/CIP-74: `SHOW <type filter> CONSTRAINTS` parses the
/// entity/kind filter surface, including the CIP-74 `PROPERTY` keyword.
#[test]
fn test_parse_show_constraints_type_filter() {
    let cases: &[(&str, ConstraintEntityFilter, Option<ConstraintKindFilter>)] = &[
        ("SHOW CONSTRAINTS", ConstraintEntityFilter::Any, None),
        ("SHOW ALL CONSTRAINTS", ConstraintEntityFilter::Any, None),
        (
            "SHOW UNIQUE CONSTRAINTS",
            ConstraintEntityFilter::Any,
            Some(ConstraintKindFilter::Unique),
        ),
        (
            "SHOW UNIQUENESS CONSTRAINTS",
            ConstraintEntityFilter::Any,
            Some(ConstraintKindFilter::Unique),
        ),
        (
            "SHOW NODE UNIQUE CONSTRAINTS",
            ConstraintEntityFilter::Node,
            Some(ConstraintKindFilter::Unique),
        ),
        (
            "SHOW RELATIONSHIP UNIQUENESS CONSTRAINTS",
            ConstraintEntityFilter::Relationship,
            Some(ConstraintKindFilter::Unique),
        ),
        (
            "SHOW REL UNIQUE CONSTRAINTS",
            ConstraintEntityFilter::Relationship,
            Some(ConstraintKindFilter::Unique),
        ),
        (
            "SHOW KEY CONSTRAINTS",
            ConstraintEntityFilter::Any,
            Some(ConstraintKindFilter::Key),
        ),
        (
            "SHOW REL KEY CONSTRAINTS",
            ConstraintEntityFilter::Relationship,
            Some(ConstraintKindFilter::Key),
        ),
        (
            "SHOW PROPERTY UNIQUENESS CONSTRAINTS",
            ConstraintEntityFilter::Any,
            Some(ConstraintKindFilter::Unique),
        ),
        (
            "SHOW NODE PROPERTY UNIQUE CONSTRAINTS",
            ConstraintEntityFilter::Node,
            Some(ConstraintKindFilter::Unique),
        ),
        (
            "SHOW RELATIONSHIP PROPERTY UNIQUENESS CONSTRAINT",
            ConstraintEntityFilter::Relationship,
            Some(ConstraintKindFilter::Unique),
        ),
        (
            "SHOW EXIST CONSTRAINTS",
            ConstraintEntityFilter::Any,
            Some(ConstraintKindFilter::Existence),
        ),
        (
            "SHOW NODE EXISTENCE CONSTRAINTS",
            ConstraintEntityFilter::Node,
            Some(ConstraintKindFilter::Existence),
        ),
        // `PROPERTY EXIST[ENCE]` is its own kind variant —
        // property existence only, never the label/endpoint-label kinds
        // the bare form also admits.
        (
            "SHOW PROPERTY EXIST CONSTRAINTS",
            ConstraintEntityFilter::Any,
            Some(ConstraintKindFilter::PropertyExistence),
        ),
        (
            "SHOW NODE PROPERTY EXISTENCE CONSTRAINTS",
            ConstraintEntityFilter::Node,
            Some(ConstraintKindFilter::PropertyExistence),
        ),
    ];
    for (query, want_entity, want_kind) in cases {
        let stmt = parse(query).unwrap();
        match &stmt.clauses[0] {
            Clause::ShowConstraints(scc) => {
                assert_eq!(scc.type_filter.entity, *want_entity, "{query}");
                assert_eq!(scc.type_filter.kind, *want_kind, "{query}");
            }
            _ => panic!("expected show constraints for {query}"),
        }
    }
}

/// CIP-74: `PROPERTY` never combines with `KEY` and never stands alone.
#[test]
fn test_parse_show_constraints_property_filter_errors() {
    assert!(parse("SHOW PROPERTY KEY CONSTRAINTS").is_err());
    assert!(parse("SHOW PROPERTY CONSTRAINTS").is_err());
}

/// CIP-11: all four relationship-pattern direction spellings resolve to
/// the same RELATIONSHIP-entity index target.
#[test]
fn test_parse_create_index_relationship_pattern_all_directions() {
    for query in [
        "CREATE RANGE INDEX ri FOR ()-[r:KNOWS]-() ON (r.since)",
        "CREATE RANGE INDEX ri FOR ()-[r:KNOWS]->() ON (r.since)",
        "CREATE RANGE INDEX ri FOR ()<-[r:KNOWS]-() ON (r.since)",
    ] {
        let stmt = parse(query).unwrap();
        match &stmt.clauses[0] {
            Clause::CreateIndex(idx) => {
                assert_eq!(idx.entity_type, SchemaEntityType::Relationship, "{query}");
                assert_eq!(idx.labels, vec!["KNOWS"], "{query}");
                assert_eq!(idx.properties, vec!["since"], "{query}");
                assert_eq!(idx.kind, IndexKind::Range, "{query}");
            }
            _ => panic!("expected create index for {query}"),
        }
    }
}

/// CIP-38/CIP-235: `FOR (n:A|B)` multi-label disjunction on a DDL
/// target.
#[test]
fn test_parse_create_index_multi_label() {
    let stmt = parse("CREATE FULLTEXT INDEX fi FOR (n:Post|Article) ON EACH [n.title]").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateIndex(idx) => {
            assert_eq!(idx.labels, vec!["Post", "Article"]);
            assert_eq!(idx.kind, IndexKind::Fulltext);
            assert!(idx.on_each);
            assert_eq!(idx.properties, vec!["title"]);
        }
        _ => panic!("expected create index"),
    }
}

/// The paren-less single-property `ON n.prop` form.
#[test]
fn test_parse_create_index_paren_less_on() {
    let stmt = parse("CREATE TEXT INDEX ti FOR (n:Book) ON n.character").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateIndex(idx) => {
            assert_eq!(idx.properties, vec!["character"]);
            assert_eq!(idx.kind, IndexKind::Text);
        }
        _ => panic!("expected create index"),
    }
}

/// `ON EACH [p.title, p.title]` is rejected as a duplicated
/// index key, not silently accepted.
#[test]
fn test_parse_create_fulltext_index_rejects_duplicate_on_each_property() {
    let err =
        parse("CREATE FULLTEXT INDEX fi FOR (p:Post) ON EACH [p.title, p.title]").unwrap_err();
    assert!(err.to_string().contains("duplicate"), "{err}");
}

/// `ON EACH []` (empty list) is rejected.
#[test]
fn test_parse_create_fulltext_index_rejects_empty_on_each() {
    assert!(parse("CREATE FULLTEXT INDEX fi FOR (p:Post) ON EACH []").is_err());
}

/// `CREATE BTREE INDEX` is a hard syntax error, not
/// silently accepted as some other kind.
#[test]
fn test_parse_create_btree_index_rejected() {
    assert!(parse("CREATE BTREE INDEX bi FOR (n:Person) ON (n.name)").is_err());
}

/// `SHOW RANGE INDEXES` / `SHOW ALL INDEXES` parse with the
/// right kind filter.
#[test]
fn test_parse_show_indexes_kind_filter() {
    let stmt = parse("SHOW RANGE INDEXES YIELD name").unwrap();
    match &stmt.clauses[0] {
        Clause::ShowIndexes(sic) => {
            assert_eq!(sic.kind_filter, Some(IndexKind::Range));
            assert_eq!(sic.yields.names().collect::<Vec<_>>(), vec!["name"]);
        }
        _ => panic!("expected show indexes"),
    }

    let stmt = parse("SHOW ALL INDEXES").unwrap();
    match &stmt.clauses[0] {
        Clause::ShowIndexes(sic) => assert_eq!(sic.kind_filter, None),
        _ => panic!("expected show indexes"),
    }
}

// -- SHOW FUNCTIONS (CIP-245/CIP-247/CIP-263) --

#[test]
fn test_parse_show_functions_with_yield_where() {
    let stmt = parse("SHOW FUNCTIONS YIELD name WHERE name = 'ceiling' RETURN name").unwrap();
    match &stmt.clauses[0] {
        Clause::ShowFunctions(sfc) => {
            assert_eq!(sfc.yields.names().collect::<Vec<_>>(), vec!["name"]);
            assert!(sfc.where_expr.is_some());
            assert!(!sfc.yields.all);
        }
        other => panic!("expected ShowFunctions, got {other:?}"),
    }
}

#[test]
fn test_parse_show_functions_bare() {
    let stmt = parse("SHOW FUNCTIONS").unwrap();
    assert_eq!(stmt.clauses.len(), 1);
    match &stmt.clauses[0] {
        Clause::ShowFunctions(sfc) => {
            assert_eq!(sfc.yields.mode(), YieldMode::Default);
            assert!(!sfc.yields.all);
            assert!(sfc.where_expr.is_none());
        }
        other => panic!("expected ShowFunctions, got {other:?}"),
    }
}

// -- SHOW USERS vs SHOW USER DEFINED FUNCTIONS dispatch carve-out (CIP-68) --

/// `SHOW USERS` / `SHOW USER` / `SHOW CURRENT USER` route to the users
/// command, never to the functions catch-all.
#[test]
fn test_parse_show_users_routing() {
    for query in ["SHOW USERS", "SHOW USER", "SHOW CURRENT USER"] {
        let stmt = parse(query).unwrap();
        assert_eq!(stmt.clauses.len(), 1, "{query}");
        match &stmt.clauses[0] {
            Clause::ShowUsers(suc) => {
                assert_eq!(suc.current_only, query == "SHOW CURRENT USER", "{query}");
            }
            other => panic!("expected ShowUsers for {query}, got {other:?}"),
        }
    }
}

/// `SHOW USER DEFINED FUNCTIONS` must fall through the users arm's
/// `!is_word(&next2, "defined")` guard and route to the functions command
/// with `FunctionScope::UserDefined`.
#[test]
fn test_parse_show_user_defined_functions_routing() {
    let stmt = parse("SHOW USER DEFINED FUNCTIONS").unwrap();
    assert_eq!(stmt.clauses.len(), 1);
    match &stmt.clauses[0] {
        Clause::ShowFunctions(sfc) => {
            assert_eq!(sfc.scope, FunctionScope::UserDefined);
        }
        other => panic!("expected ShowFunctions, got {other:?}"),
    }
}

// `OPTIONS $param` is a legal spelling wherever `OPTIONS
// { ... }` is -- on all three CREATE CONSTRAINT verb paths, on CREATE INDEX,
// and inside a graph-type body. The parameter resolves at execution time.
#[test]
fn test_parse_create_constraint_options_parameter() {
    for (query, want_param) in [
        (
            "CREATE CONSTRAINT FOR (n:L) REQUIRE (n.p) IS NODE KEY OPTIONS $opts",
            "opts",
        ),
        (
            "CREATE CONSTRAINT my_c FOR (n:L) REQUIRE (n.p) IS UNIQUE OPTIONS $opts",
            "opts",
        ),
        (
            "CREATE CONSTRAINT FOR ()-[r:R]-() REQUIRE (r.p) IS NOT NULL OPTIONS $opts",
            "opts",
        ),
        (
            "CREATE CONSTRAINT FOR (n:L) REQUIRE n.p IS :: INTEGER OPTIONS $opts",
            "opts",
        ),
        (
            "CREATE INDEX my_idx FOR (n:L) ON (n.p) OPTIONS $opts",
            "opts",
        ),
    ] {
        let stmt = parse(query).unwrap_or_else(|e| panic!("{query}: {e}"));
        let options = match &stmt.clauses[0] {
            Clause::CreateConstraint(c) => &c.options,
            Clause::CreateIndex(c) => &c.options,
            other => panic!("{query}: expected a create clause, got {other:?}"),
        };
        assert!(
            options.literal().is_none(),
            "{query}: a parameter form carries no literal options"
        );
        assert_eq!(options.parameter(), Some(want_param), "{query}");
    }
}

#[test]
fn test_parse_graph_type_constraint_options_parameter() {
    let stmt = parse(
        "CREATE GRAPH TYPE gt AS {
            (n:L => {p1 :: STRING})
              REQUIRE n.p2 IS KEY OPTIONS $emptyMap
              REQUIRE n.p3 IS UNIQUE OPTIONS $configMap
        }",
    )
    .unwrap();
    match &stmt.clauses[0] {
        Clause::CreateGraphType(c) => {
            let params: Vec<_> = c
                .body
                .constraints
                .iter()
                .map(|constraint| constraint.options.parameter())
                .collect();
            assert_eq!(params, vec![Some("emptyMap"), Some("configMap")]);
            assert!(c
                .body
                .constraints
                .iter()
                .all(|constraint| constraint.options.literal().is_none()));
        }
        other => panic!("expected CreateGraphType clause, got {other:?}"),
    }
}

/// A LOOKUP index's `ON EACH labels(...)`/`type(...)` argument must be the
/// pattern's own variable; any other name is undefined in this scope, and
/// the rejection carries the `UndefinedVariable` detail tag every other
/// undefined-variable site has — it is what lifts `EngineError::kind()` off
/// the generic `UnexpectedSyntax` parse fallback.
#[test]
fn test_parse_lookup_index_rejects_foreign_variable() {
    for query in [
        "CREATE LOOKUP INDEX FOR (n) ON EACH labels(x)",
        "CREATE LOOKUP INDEX FOR ()-[r]-() ON EACH type(x)",
    ] {
        let err = parse(query).unwrap_err();
        assert_eq!(err.message, "Variable `x` not defined.", "{query}");
        assert_eq!(
            err.detail,
            Some(crate::parser::UNDEFINED_VARIABLE_DETAIL),
            "{query}"
        );
    }
    // The pattern's own variable stays accepted.
    parse("CREATE LOOKUP INDEX FOR (n) ON EACH labels(n)").unwrap();
    parse("CREATE LOOKUP INDEX FOR ()-[r]-() ON EACH type(r)").unwrap();
}

// A FOR target must name exactly one label or relationship type: a
// label-less target is rejected rather than defaulted to an empty name.
#[test]
fn test_create_constraint_requires_one_label() {
    for q in [
        "CREATE CONSTRAINT c FOR (n) REQUIRE n.x IS UNIQUE",
        "CREATE CONSTRAINT c FOR ()-[r]-() REQUIRE r.x IS UNIQUE",
    ] {
        let err = parse(q).unwrap_err();
        assert!(
            err.message.contains("require a label or relationship type"),
            "{q:?}: {}",
            err.message
        );
    }
    let err = parse("CREATE CONSTRAINT c FOR (n:A|B) REQUIRE n.x IS UNIQUE").unwrap_err();
    assert!(
        err.message.contains("only a single label"),
        "{}",
        err.message
    );
}
