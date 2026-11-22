//! The pinned Neo4j CE error-text suffixes for the fixed-namespace tests
//! (testdata/ce-errors/neo4j-ce.json) and the authored not-queryable text.

/// CE 42N17 suffix (ADR-0116 §4; testdata/ce-errors/neo4j-ce.json
/// `system_data_clause`).
pub const SYSTEM_CLAUSES_SUFFIX: &str = "The system database supports a restricted set of Cypher clauses. The supported clause structure for procedure calls is: CALL, YIELD, RETURN. YIELD and RETURN clauses are optional. The order of the clauses is fixed and each can only occur once.";
/// CE 42N18 text (ADR-0116 §4/§9; CE appends "on this Neo4j instance." to
/// the ADR wording — testdata/ce-errors/neo4j-ce.json `read_only_write`).
/// Still the recorded CE text for read-only databases; the
/// `system_ontologies` gate no longer uses it (ADR-0229 §2).
pub const READ_ONLY_SUFFIX: &str =
    "No write operations are allowed on this database. The database is in read-only mode on this Neo4j instance.";
/// The authored 42N17 text every non-administration statement routed to
/// `system_ontologies` fails with (ADR-0229 §2: reads included,
/// EXPLAIN-wrapped included). Not CE-pinned — live CE allows reads on the
/// namespace, so the fixture carries no case for it.
pub const NOT_QUERYABLE_SUFFIX: &str =
    "This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead.";
/// CE 2026.09's composability text for a `WITH` around a
/// database-administration command on the `system` namespace, live-pinned
/// 2026-11-04 (ADR-0116 §4 completion; testdata/ce-errors/neo4j-ce.json
/// `use_system_show_databases_with`; wire 42001 /
/// Neo.ClientError.Statement.SemanticError, cause chain 42N17 → 42NA9).
pub const WITH_ON_SYSTEM_SUFFIX: &str =
    "The following clauses are not allowed on a system database: WITH.";
/// CE 2026.09's 42N79 auto-routing text, live-pinned 2026-11-04 (ADR-0116
/// §2 amendment; testdata/ce-errors/neo4j-ce.json
/// `use_neo4j_show_databases`; Neo.ClientError.Statement.SyntaxError).
pub const USE_WITH_ADMIN_SUFFIX: &str =
    "The `USE` clause is not required for administration commands. \
     Retry your query without the `USE` clause, and it will be routed automatically.";
/// CE's off-system 51N28 text, live-verified 2026-10 (ADR-0590;
/// Neo.ClientError.Statement.NotSystemDatabaseError). The refused command
/// is appended by the caller.
pub const NEEDS_SYSTEM_DATABASE_SUFFIX: &str =
    "This is an administration command and it should be executed against the system database: ";
