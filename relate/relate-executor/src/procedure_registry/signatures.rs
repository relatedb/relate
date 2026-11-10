//! The canonical built-in procedure table: the single source of every
//! procedure's signature and catalog metadata, kept under the file-size rule
//! by the compact constructors.

use super::{ProcSignature, RowProducer};
use crate::catalog_meta::{DeprecatedBy, ProcArgMeta};

/// The identical return-column list `db.info` and `dbms.info` share
/// (ADR-0142): the deliberately common `id`/`name`/`creationDate` output
/// shape, expressed once so the two entries cannot drift apart.
const DB_INFO_RETURNS: &[ProcArgMeta] = &[
    ProcArgMeta::new("id", "STRING"),
    ProcArgMeta::new("name", "STRING"),
    ProcArgMeta::new("creationDate", "STRING"),
];

/// The identical 8-column `ConfigResult` shape `dbms.clientConfig` and
/// `dbms.listConfig` share (ADR-0143): the Neo4j 5.x+ documented output
/// shape, expressed once so the two entries cannot drift apart.
const CONFIG_RESULT_RETURNS: &[ProcArgMeta] = &[
    ProcArgMeta::new("name", "STRING"),
    ProcArgMeta::new("description", "STRING"),
    ProcArgMeta::new("value", "STRING"),
    ProcArgMeta::new("dynamic", "BOOLEAN"),
    ProcArgMeta::new("defaultValue", "STRING"),
    ProcArgMeta::new("startupValue", "STRING"),
    ProcArgMeta::new("explicitlySet", "BOOLEAN"),
    ProcArgMeta::new("validValues", "STRING"),
];

/// The similarity-score column both vector-index query procedures return.
const VECTOR_QUERY_SCORE: ProcArgMeta =
    ProcArgMeta::described("score", "The similarity score.", "FLOAT");

/// The argument list both vector-index query procedures share.
const VECTOR_QUERY_ARGS: &[ProcArgMeta] = &[
    ProcArgMeta::described("indexName", "The name of the vector index.", "STRING"),
    ProcArgMeta::described(
        "numberOfNearestNeighbours",
        "The number of nearest neighbors to return.",
        "INTEGER",
    ),
    ProcArgMeta::described("query", "The query vector.", "LIST<FLOAT>"),
];

/// The `timeOutSeconds` argument `db.awaitIndex` and `db.awaitIndexes`
/// share.
const AWAIT_INDEX_TIMEOUT: ProcArgMeta = ProcArgMeta::with_default(
    "timeOutSeconds",
    "The maximum time to wait, in seconds.",
    "INTEGER",
    "300",
);

/// The canonical built-in procedure table, in name order — the order
/// `SHOW PROCEDURES` lists the `listed` rows in (ADR-0497 §6).
///
/// ADR-0497 §6 aligns the table with the Cypher-25 corpus row by row:
/// descriptions, modes and `worksOnSystem` flags are the corpus's text and
/// values, `db.indexes`/`db.constraints` are gone (the corpus has no row
/// and `SHOW INDEXES`/`SHOW CONSTRAINTS` are the surface), and the
/// `catalog-only` entries below are inventory rows the corpus lists but
/// never calls — listed, with no engine operation behind them.
pub static BUILTIN_PROCEDURE_SIGNATURES: &[ProcSignature] = &[
    // CIP-123 boundaries[5]: Neo4j 2026.05 still lists the pre-CIP-79
    // singular `db.awaitIndex` alongside the current plural
    // `db.awaitIndexes` (see the CIP-123 doc's own worked "Combining all
    // commands" example table, which lists both as distinct real
    // procedures) -- kept here purely as a catalog entry, like every other
    // `SHOW PROCEDURES`-only name in this list.
    ProcSignature::read("db.awaitIndex")
        .works_on_system()
        .described("Wait for an index to come online (for example: CALL db.awaitIndex(\"MyIndex\", 300)).")
        .args(&[
            ProcArgMeta::described("indexName", "The name of the index to await.", "STRING"),
            AWAIT_INDEX_TIMEOUT,
        ])
        .executable(false),
    ProcSignature::read("db.awaitIndexes")
        .works_on_system()
        .described("Wait for all indexes to come online (for example: CALL db.awaitIndexes(300)).")
        .args(&[AWAIT_INDEX_TIMEOUT]),
    ProcSignature::dbms("db.checkpoint")
        .works_on_system()
        .described("Initiate and wait for a new check point, or wait any already on-going check point to complete. Note that this temporarily disables the `db.checkpoint.iops.limit` setting in order to make the check point complete faster. This might cause transaction throughput to degrade slightly, due to increased IO load.")
        .returns(&[
            ProcArgMeta::described("success", "Whether the checkpoint succeeded.", "BOOLEAN"),
            ProcArgMeta::described("message", "A human-readable status message.", "STRING"),
        ])
        .executable(false),
    ProcSignature::dbms("db.clearQueryCaches")
        .admin()
        .works_on_system()
        .described("Clears all query caches.")
        .executable(false),
    ProcSignature::write("db.createLabel")
        .described("Create a label")
        .writes()
        .args(&[ProcArgMeta::new("newLabel", "STRING")]),
    ProcSignature::write("db.createProperty")
        .described("Create a Property")
        .executable(false),
    ProcSignature::write("db.createRelationshipType")
        .described("Create a RelationshipType")
        .executable(false),
    // CIP-113 procedure-signatures.feature [6]/[16]: no input
    // parameters, no return columns -- the reachable `::VOID`-
    // replacement (no suffix at all) case.
    ProcSignature::read("db.index.fulltext.awaitEventuallyConsistentIndexRefresh")
        .works_on_system()
        .described("Wait for the updates from recently committed transactions to be applied to any eventually-consistent full-text indexes.")
        .executable(false),
    // CIP-113 procedure-signatures.feature [11]: a `LIST<STRING>` return
    // column (the old `LIST? OF STRING` -> `LIST<STRING>` rename).
    ProcSignature::read("db.index.fulltext.listAvailableAnalyzers")
        .works_on_system()
        .described("List the available analyzers that the full-text indexes can be configured with.")
        .returns(&[
            ProcArgMeta::described("analyzer", "The name of the analyzer.", "STRING"),
            ProcArgMeta::described("description", "A description of the analyzer.", "STRING"),
            ProcArgMeta::described("stopwords", "The stopwords used by the analyzer.", "LIST<STRING>"),
        ])
        .executable(false),
    // Cypher 25 deprecates the fulltext query procedures in favour of the
    // CIP-224 `SEARCH` clause, exactly like their vector counterparts below.
    ProcSignature::read("db.index.fulltext.queryNodes")
        .works_on_system()
        .described(
            "Query the given full-text index. Returns the matching nodes and their Lucene query score, ordered by score.\n\
             Valid _key: value_ pairs for the `options` map are:\n\
             \n\
             * 'skip' -- to skip the top N results.\n\
             * 'limit' -- to limit the number of results returned.\n\
             * 'analyzer' -- to use the specified analyzer as a search analyzer for this query.\n\
             \n\
             The `options` map and any of the keys are optional.\n\
             An example of the `options` map: `{skip: 30, limit: 10, analyzer: 'whitespace'}`\n",
        )
        .args(&[
            ProcArgMeta::new("indexName", "STRING"),
            ProcArgMeta::new("query", "STRING"),
        ])
        .returns(&[
            ProcArgMeta::new("node", "NODE"),
            ProcArgMeta::new("score", "FLOAT"),
        ])
        .deprecated_by(DeprecatedBy::clause("SEARCH")),
    ProcSignature::read("db.index.fulltext.queryRelationships")
        .works_on_system()
        .described(
            "Query the given full-text index. Returns the matching relationships and their Lucene query score, ordered by score.\n\
             Valid _key: value_ pairs for the `options` map are:\n\
             \n\
             * 'skip' -- to skip the top N results.\n\
             * 'limit' -- to limit the number of results returned.\n\
             * 'analyzer' -- to use the specified analyzer as a search analyzer for this query.\n\
             \n\
             The `options` map and any of the keys are optional.\n\
             An example of the `options` map: `{skip: 30, limit: 10, analyzer: 'whitespace'}`\n",
        )
        .args(&[
            ProcArgMeta::new("indexName", "STRING"),
            ProcArgMeta::new("query", "STRING"),
        ])
        .returns(&[
            ProcArgMeta::new("relationship", "RELATIONSHIP"),
            ProcArgMeta::new("score", "FLOAT"),
        ])
        .deprecated_by(DeprecatedBy::clause("SEARCH")),
    // CIP-84/CIP-157: deprecated in favor of the CIP-224 `SEARCH` clause.
    ProcSignature::read("db.index.vector.queryNodes")
        .described(
            "Query the given node vector index.\n\
             Returns requested number of nearest neighbors to the provided query vector,\n\
             and their similarity score to that query vector, based on the configured similarity function for the index.\n\
             The similarity score is a value between [0, 1]; where 0 indicates least similar, 1 most similar.\n",
        )
        .args(VECTOR_QUERY_ARGS)
        .returns(&[
            ProcArgMeta::described("node", "A matched node.", "NODE"),
            VECTOR_QUERY_SCORE,
        ])
        .deprecated_by(DeprecatedBy::clause("SEARCH")),
    ProcSignature::read("db.index.vector.queryRelationships")
        .described(
            "Query the given relationship vector index.\n\
             Returns requested number of nearest neighbors to the provided query vector,\n\
             and their similarity score to that query vector, based on the configured similarity function for the index.\n\
             The similarity score is a value between [0, 1]; where 0 indicates least similar, 1 most similar.\n",
        )
        .args(VECTOR_QUERY_ARGS)
        .returns(&[
            ProcArgMeta::described("relationship", "A matched relationship.", "RELATIONSHIP"),
            VECTOR_QUERY_SCORE,
        ])
        .deprecated_by(DeprecatedBy::clause("SEARCH")),
    ProcSignature::read("db.info")
        .works_on_system()
        .described("Provides information regarding the database.")
        .returns(DB_INFO_RETURNS)
        .rows(RowProducer::DbInfo),
    ProcSignature::read("db.labels")
        .works_on_system()
        .described("List all labels attached to nodes within a database according to the user's access rights. The procedure returns empty results if the user is not authorized to view those labels.")
        .returns(&[ProcArgMeta::described("label", "A label within the database.", "STRING")])
        .rows(RowProducer::DbLabels),
    ProcSignature::dbms("db.listLocks")
        .admin()
        .works_on_system()
        .described("List all locks at this database.")
        .executable(false),
    // ADR-0121 as amended by ADR-0227 (CIP-287): the ontology procedures.
    // No database argument — each acts on the session/USE-target database,
    // which must be `neo4j`. `set`/`drop` are store-level ops (facade
    // gates them to implicit transactions and forbids mixed writes, ADR-0121
    // §3) whose redo the executor pushes; `apply` is an ordinary in-overlay
    // write; `get` is a read. `writes: true` on set/drop/apply routes their
    // statements to the write path (the facade's `write_route`, ADR-0121);
    // `store_level: true` on set/drop alone is the store-op classification
    // the implicit-only / no-mixed-writes gates read (ADR-0120 §3);
    // statements routed to `system_ontologies` fail ADR-0229 §1's gate
    // before `writes` could matter there.
    ProcSignature::write("db.ontology.apply")
        .described("Convert the stored ontology to a graph type and apply it to the database with ALTER CURRENT GRAPH TYPE SET semantics, failing when existing data violates it.")
        .writes(),
    ProcSignature::write("db.ontology.drop")
        .described("Drop the stored ontology document. Succeeds when none is set.")
        .writes()
        .store_level(),
    ProcSignature::read("db.ontology.get")
        .described("Return the stored ontology document in the requested format, or null when none is set.")
        .args(&[ProcArgMeta::with_default("format", "The format to return the ontology in: 'yaml' (default) or 'json'.", "STRING", "\"yaml\"")])
        .returns(&[ProcArgMeta::described("ontology", "The stored ontology document in the requested format.", "STRING")]),
    ProcSignature::write("db.ontology.set")
        .described("Validate and store the ontology document (JSON or YAML) of the database, replacing any current one.")
        .writes()
        .store_level()
        .args(&[ProcArgMeta::described("ontology", "The ontology document, in JSON or YAML.", "STRING")]),
    ProcSignature::read("db.ping")
        .works_on_system()
        .described("This procedure can be used by client side tooling to test whether they are correctly connected to a database. The procedure is available in all databases and always returns true. A faulty connection can be detected by not being able to call this procedure.")
        .returns(&[ProcArgMeta::described("success", "Whether the database answered.", "BOOLEAN")])
        .rows(RowProducer::DbPing),
    ProcSignature::read("db.prepareForReplanning")
        .admin()
        .works_on_system()
        .described("Triggers an index resample and waits for its completion. If the wait time is 0, it skips waiting. After that, the procedure clears query caches. Once the process is complete, queries are planned using the latest database statistics.")
        .executable(false),
    ProcSignature::read("db.propertyKeys")
        .works_on_system()
        .described("List all property keys in the database.")
        .returns(&[ProcArgMeta::new("propertyKey", "STRING")])
        .rows(RowProducer::DbPropertyKeys),
    ProcSignature::read("db.relationshipTypes")
        .works_on_system()
        .described("List all types attached to relationships within a database according to the user's access rights. The procedure returns empty results if the user is not authorized to view those relationship types.")
        .returns(&[ProcArgMeta::new("relationshipType", "STRING")])
        .rows(RowProducer::DbRelationshipTypes),
    ProcSignature::read("db.schema.nodeTypeProperties")
        .works_on_system()
        .described("Show the derived property schema of the nodes in tabular form.")
        .returns(&[
            ProcArgMeta::described("nodeType", "The node labels rendered as colon-prefixed, sorted labels.", "STRING"),
            ProcArgMeta::described("nodeLabels", "The sorted labels attached to the node type.", "LIST<STRING>"),
            ProcArgMeta::described("propertyName", "A property key found on the node type.", "STRING"),
            ProcArgMeta::described("propertyTypes", "The distinct value types observed for the property key.", "LIST<STRING>"),
            ProcArgMeta::described("mandatory", "Whether the property key is present on every node of the type.", "BOOLEAN"),
        ])
        .rows(RowProducer::SchemaNodeTypeProperties),
    ProcSignature::read("db.schema.relTypeProperties")
        .works_on_system()
        .described("Show the derived property schema of the relationships in tabular form.")
        .returns(&[
            ProcArgMeta::described("relType", "The relationship type rendered in bracket form.", "STRING"),
            ProcArgMeta::described("propertyName", "A property key found on the relationship type.", "STRING"),
            ProcArgMeta::described("propertyTypes", "The distinct value types observed for the property key.", "LIST<STRING>"),
            ProcArgMeta::described("mandatory", "Whether the property key is present on every relationship of the type.", "BOOLEAN"),
        ])
        .rows(RowProducer::SchemaRelTypeProperties),
    // LABS-150: Neo4j's schema graph for Browser/Bloom-style sidebars,
    // inferred by scanning stored entities (ADR-0112's rule). Listed by
    // `SHOW PROCEDURES` exactly like its `db.schema.*` siblings.
    // LABS-389 (CIP-277 compat smoke): the call shape with an options
    // map is accepted, not failed. A **null** map selects nothing at all
    // (Neo4j yields zero rows rather than the whole stored schema); a
    // non-null map's contents are ignored -- the visualization's own
    // output is parameter-free (the stored entities are the whole truth,
    // ADR-0112). A missing argument keeps the whole-schema output, and
    // Neo4j parity keeps the zero-arg call the documented form.
    ProcSignature::read("db.schema.visualization")
        .works_on_system()
        .described("Visualizes the schema of the data based on available statistics. A new node is returned for each label. The properties represented on the node include: `name` (label name), `indexes` (list of indexes), and `constraints` (list of constraints). A relationship of a given type is returned for all possible combinations of start and end nodes. The properties represented on the relationship include: `name` (type name). Note that this may include additional relationships that do not exist in the data due to the information available in the count store. Set the flag to false to get more accurate results. Setting the flag to false could affect performance.")
        .args(&[ProcArgMeta::with_default(
            "options",
            "A boolean chooses synthesized (ghost) relationships: true (the default)              includes every label combination of a relationship type, false only the              combinations that exist in the data. The legacy map form is equivalent to              true and its contents are ignored; a null argument selects nothing.",
            "BOOLEAN|MAP",
            "true",
        )])
        .returns(&[
            ProcArgMeta::described("nodes", "One virtual node per stored label.", "LIST<NODE>"),
            ProcArgMeta::described("relationships", "One virtual relationship per distinct (startLabel, relType, endLabel) triple.", "LIST<RELATIONSHIP>"),
        ])
        .rows(RowProducer::SchemaVisualization),
    // ADR-0497 §6: the one added procedure the corpus calls -- the GRAPH
    // COUNTS diagnostics section, derived from the live constraints,
    // indexes and graph type rather than catalogued alone.
    ProcSignature::read("db.stats.retrieve")
        .admin()
        .works_on_system()
        .described("Retrieve statistical data about the current database. Valid sections are 'GRAPH COUNTS', 'TOKENS', 'QUERIES', 'META'")
        .args(&[ProcArgMeta::described("section", "The section of statistics to retrieve.", "STRING")])
        .returns(&[
            ProcArgMeta::described("section", "The section this row of statistics belongs to.", "STRING"),
            ProcArgMeta::described("data", "The requested section's statistics.", "MAP"),
        ])
        .rows(RowProducer::DbStatsRetrieve),
    // ADR-0143: `dbms.clientConfig` and `dbms.listConfig` share the Neo4j
    // 5.x+ `ConfigResult` shape (`CONFIG_RESULT_RETURNS`) by design.
    // ADR-0497 §6: `dbms.clientConfig` stays callable but unlisted -- the
    // corpus inventory has no row for it.
    ProcSignature::dbms("dbms.clientConfig")
        .works_on_system()
        .described("List the configuration settings of clients currently connected to this instance.")
        .returns(CONFIG_RESULT_RETURNS)
        .listed(false)
        .rows(RowProducer::DbmsConfig),
    ProcSignature::dbms("dbms.components")
        .works_on_system()
        .described("List DBMS components and their versions.")
        .returns(&[
            ProcArgMeta::new("name", "STRING"),
            ProcArgMeta::new("versions", "LIST<STRING>"),
            // ADR-0142: Browser-compat column; Relate has no enterprise
            // tier, so every row reports `community`.
            ProcArgMeta::new("edition", "STRING"),
        ])
        .rows(RowProducer::DbmsComponents),
    // ADR-0142: Neo4j Desktop calls `dbms.info()` on connect
    // (`updateDBMSInfo`). Shares `db.info`'s return columns
    // (`DB_INFO_RETURNS`) by design.
    ProcSignature::dbms("dbms.info")
        .works_on_system()
        .described("Provides information regarding the DBMS.")
        .returns(DB_INFO_RETURNS)
        .rows(RowProducer::DbmsInfo),
    // ADR-0143: an honest constant row (Apache-2.0 has no agreement to
    // accept and no trial); ADR-0497 §6 keeps it callable but unlisted --
    // the corpus inventory has no row for it.
    ProcSignature::dbms("dbms.licenseAgreementDetails")
        .works_on_system()
        .described("Show the status of the license agreement for this instance.")
        .returns(&[
            ProcArgMeta::new("status", "STRING"),
            ProcArgMeta::new("daysLeftOnTrial", "INTEGER"),
            ProcArgMeta::new("totalTrialDays", "INTEGER"),
        ])
        .listed(false)
        .rows(RowProducer::DbmsLicenseAgreementDetails),
    // The capability-name vocabulary is vendor-owned: Neo4j's own rows
    // describe its Graph Engine and its JDBC drivers, which Relate has none
    // of. Rather than mirror rows it cannot honour, this procedure reports
    // the capabilities this engine actually has (ADR-0558).
    ProcSignature::dbms("dbms.listCapabilities")
        .described("List capabilities.")
        .returns(&[
            ProcArgMeta::described("name", "The capability name.", "STRING"),
            ProcArgMeta::described("description", "A description of the capability.", "STRING"),
            ProcArgMeta::described("value", "The capability value.", "ANY"),
        ])
        .works_on_system()
        .rows(RowProducer::DbmsListCapabilities),
    ProcSignature::dbms("dbms.listConfig")
        .admin()
        .works_on_system()
        .described("List the currently active configuration settings of Neo4j.")
        .returns(CONFIG_RESULT_RETURNS)
        .rows(RowProducer::DbmsConfig),
    // ADR-0143: Browser/Desktop-compat procedure (LABS-148). Fixed
    // documented-placeholder columns — the engine carries no session
    // identity beyond the connection principal (ADR-0273).
    ProcSignature::dbms("dbms.showCurrentUser")
        .works_on_system()
        .described("Show the current user.")
        .returns(&[
            ProcArgMeta::new("username", "STRING"),
            ProcArgMeta::new("roles", "LIST<STRING>"),
            ProcArgMeta::new("flags", "LIST<STRING>"),
        ])
        .rows(RowProducer::DbmsShowCurrentUser),
    ProcSignature::dbms("tx.getMetaData")
        .works_on_system()
        .described("Provides attached transaction metadata.")
        .returns(&[ProcArgMeta::new("metadata", "MAP")])
        .rows(RowProducer::TxGetMetaData),
    // ADR-0497 §6: the corpus lists `tx.setMetaData` as DBMS-mode but not
    // system-database-capable, so `works_on_system` stays off.
    ProcSignature::dbms("tx.setMetaData")
        .described("Attaches a map of data to the transaction. The data will be printed when listing queries, and inserted into the query log.")
        .args(&[ProcArgMeta::new("metadata", "MAP")]),
];
