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

/// The routing-table argument list `dbms.routing.getRoutingTable` and its
/// cluster-prefixed predecessor share (ADR-0143), so the alias and its
/// replacement cannot drift apart.
const ROUTING_TABLE_ARGS: &[ProcArgMeta] = &[
    ProcArgMeta::described("context", "Client-provided routing context.", "MAP"),
    ProcArgMeta::with_default(
        "database",
        "The database to get a routing table for.",
        "STRING",
        "null",
    ),
];

/// The routing-table return columns the two `getRoutingTable` entries share.
const ROUTING_TABLE_RETURNS: &[ProcArgMeta] = &[
    ProcArgMeta::described(
        "ttl",
        "The number of seconds this routing table is valid for.",
        "INTEGER",
    ),
    ProcArgMeta::described(
        "servers",
        "The servers making up the routing table, grouped by their role.",
        "LIST<MAP>",
    ),
];

/// The routing-table description the two `getRoutingTable` entries share
/// (ADR-0497 §6: the corpus lists the same text for both).
const ROUTING_TABLE_DESCRIPTION: &str =
    "Returns the advertised bolt capable endpoints for a given database, divided by each endpoint's capabilities. For example, an endpoint may serve read queries, write queries, and/or future `getRoutingTable` requests.";

/// The change-identifier return column the `cdc.current`/`db.cdc.current`
/// pair share.
const CDC_CURRENT_RETURNS: &[ProcArgMeta] = &[ProcArgMeta::described(
    "id",
    "The current change identifier.",
    "STRING",
)];

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
    // CIP-157: deprecated in favor of the `db.cdc.*` namespace.
    ProcSignature::read("cdc.current")
        .described("Returns the current change identifier that can be used to stream changes from.")
        .returns(CDC_CURRENT_RETURNS)
        .deprecated_by(DeprecatedBy::identifier("db.cdc.current"))
        .executable(false),
    ProcSignature::read("cdc.earliest")
        .described("Returns the earliest change identifier that can be used to stream changes from.")
        .returns(&[ProcArgMeta::described("id", "The earliest available change identifier.", "STRING")])
        .deprecated_by(DeprecatedBy::identifier("db.cdc.earliest"))
        .executable(false),
    ProcSignature::read("cdc.query")
        .admin()
        .described("Query changes happened from the provided change identifier.")
        .args(&[
            ProcArgMeta::described("from", "The change identifier to query from.", "STRING"),
            ProcArgMeta::with_default("selectors", "Selectors narrowing which changes to return.", "LIST<MAP>", "[]"),
        ])
        .returns(&[
            ProcArgMeta::described("id", "The change identifier of this event.", "STRING"),
            ProcArgMeta::described("txId", "The id of the transaction that produced this event.", "INTEGER"),
            ProcArgMeta::described("seq", "The sequence number of this event within its transaction.", "INTEGER"),
            ProcArgMeta::described("metadata", "Metadata about the transaction that produced this event.", "MAP"),
            ProcArgMeta::described("event", "The change event itself.", "MAP"),
        ])
        .deprecated_by(DeprecatedBy::identifier("db.cdc.query"))
        .executable(false),
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
    ProcSignature::read("db.cdc.current")
        .described("Returns the current change identifier and commit time that can be used to stream changes from.")
        .returns(CDC_CURRENT_RETURNS)
        .executable(false),
    ProcSignature::read("db.cdc.earliest")
        .described("Returns the earliest change identifier that can be used to stream changes from.")
        .executable(false),
    ProcSignature::read("db.cdc.query")
        .admin()
        .described("Query changes happened from the provided change identifier.")
        .executable(false),
    ProcSignature::read("db.cdc.translateId")
        .admin()
        .described("Translate any provided change identifier to match the queried database.")
        .executable(false),
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
    ProcSignature::write("db.create.setNodeVectorProperty")
        .described("Set a vector property on a given node in a more space efficient representation than Cypher's SET.")
        .executable(false),
    ProcSignature::write("db.create.setRelationshipVectorProperty")
        .described("Set a vector property on a given relationship in a more space efficient representation than Cypher's SET.")
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
    ProcSignature::read("db.resampleIndex")
        .works_on_system()
        .described("Schedule resampling of an index (for example: CALL db.resampleIndex(\"MyIndex\")).")
        .args(&[ProcArgMeta::described("indexName", "The name of the index to resample.", "STRING")])
        .executable(false),
    ProcSignature::read("db.resampleOutdatedIndexes")
        .works_on_system()
        .described("Schedule resampling of all outdated indexes.")
        .executable(false),
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
    ProcSignature::read("db.stats.clear")
        .admin()
        .works_on_system()
        .described("Clear collected data of a given data section. Valid sections are 'QUERIES'")
        .executable(false),
    // CIP-113 procedure-signatures.feature [1]: the doc's own literal
    // worked example.
    ProcSignature::read("db.stats.collect")
        .admin()
        .works_on_system()
        .described("Start data collection of a given data section. Valid sections are 'QUERIES'")
        .args(&[
            ProcArgMeta::described("section", "The section of statistics to collect.", "STRING"),
            ProcArgMeta::with_default("config", "Configuration for the collection.", "MAP", "{}"),
        ])
        .returns(&[
            ProcArgMeta::described("section", "The section this row of statistics belongs to.", "STRING"),
            ProcArgMeta::described("success", "Whether collection for this section succeeded.", "BOOLEAN"),
            ProcArgMeta::described("message", "A human-readable status message.", "STRING"),
        ])
        .executable(false),
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
    ProcSignature::read("db.stats.retrieveAllAnonymized")
        .admin()
        .works_on_system()
        .described("Retrieve all available statistical data about the current database, in an anonymized form.")
        .executable(false),
    ProcSignature::read("db.stats.status")
        .admin()
        .works_on_system()
        .described("Retrieve the status of all available collector daemons, for this database.")
        .executable(false),
    ProcSignature::read("db.stats.stop")
        .admin()
        .works_on_system()
        .described("Stop data collection of a given data section. Valid sections are 'QUERIES'")
        .executable(false),
    ProcSignature::dbms("dbms.checkConfigValue")
        .admin()
        .works_on_system()
        .described("Check if a potential config setting value is valid.")
        .executable(false),
    ProcSignature::dbms("dbms.cluster.checkConnectivity")
        .admin()
        .works_on_system()
        .described("Check the connectivity of this instance to other cluster members. Not all ports are relevant to all members. Valid values for 'port-name' are: []")
        .executable(false),
    ProcSignature::write("dbms.cluster.cordonServer")
        .works_on_system()
        .described("Mark a server in the topology as not suitable for new allocations. It will not force current allocations off the server. This is useful when deallocating databases when you have multiple unavailable servers.")
        .executable(false),
    ProcSignature::write("dbms.cluster.deallocateDatabaseFromServer")
        .works_on_system()
        .described("Deallocate a specific user database from a specific server.")
        .executable(false),
    ProcSignature::write("dbms.cluster.deallocateDatabaseFromServers")
        .works_on_system()
        .described("Deallocate a specific user database from a list of servers.")
        .executable(false),
    ProcSignature::write("dbms.cluster.deallocateNumberOfDatabases")
        .works_on_system()
        .described("Deallocate a number of user databases from a specific server.")
        .executable(false),
    ProcSignature::dbms("dbms.cluster.protocols")
        .works_on_system()
        .described("Overview of installed protocols")
        .executable(false),
    ProcSignature::write("dbms.cluster.reallocateDatabase")
        .works_on_system()
        .described("Reallocate a specific database.")
        .executable(false),
    ProcSignature::write("dbms.cluster.reallocateNumberOfDatabases")
        .works_on_system()
        .described("Reallocate a specified number of user databases.")
        .executable(false),
    ProcSignature::write("dbms.cluster.recreateDatabase")
        .works_on_system()
        .described("Recreates a database while keeping all RBAC settings. The procedure initiates a process, which when complete, will have synchronized and started all database instances within the cluster.")
        .args(&[ProcArgMeta::described("databaseName", "The name of the database to re-seed.", "STRING")])
        .deprecated_by(DeprecatedBy::identifier("dbms.recreateDatabase"))
        .executable(false),
    ProcSignature::dbms("dbms.cluster.routing.getRoutingTable")
        .works_on_system()
        .described(ROUTING_TABLE_DESCRIPTION)
        .args(ROUTING_TABLE_ARGS)
        .returns(ROUTING_TABLE_RETURNS)
        .deprecated_by(DeprecatedBy::identifier("dbms.routing.getRoutingTable"))
        .executable(false),
    ProcSignature::dbms("dbms.cluster.secondaryReplicationDisable")
        .admin()
        .works_on_system()
        .described("The toggle can pause or resume the secondary replication process.")
        .executable(false),
    ProcSignature::write("dbms.cluster.setAutomaticallyEnableFreeServers")
        .works_on_system()
        .described("With this method you can set whether free servers are automatically enabled.")
        .executable(false),
    ProcSignature::dbms("dbms.cluster.statusCheck")
        .admin()
        .works_on_system()
        .described("Performs a rafted status check.")
        .executable(false),
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
    ProcSignature::dbms("dbms.killConnection")
        .works_on_system()
        .described("Kill network connection with the given connection id.")
        .executable(false),
    ProcSignature::dbms("dbms.killConnections")
        .works_on_system()
        .described("Kill all network connections with the given connection ids.")
        .executable(false),
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
    ProcSignature::dbms("dbms.listActiveLocks")
        .works_on_system()
        .described("List the active lock requests granted for the transaction executing the query with the given query id.")
        .executable(false),
    ProcSignature::dbms("dbms.listCapabilities")
        .works_on_system()
        .described("List capabilities.")
        .executable(false),
    ProcSignature::dbms("dbms.listConfig")
        .admin()
        .works_on_system()
        .described("List the currently active configuration settings of Neo4j.")
        .returns(CONFIG_RESULT_RETURNS)
        .rows(RowProducer::DbmsConfig),
    ProcSignature::dbms("dbms.listConnections")
        .works_on_system()
        .described("List all accepted network connections at this instance that are visible to the user.")
        .executable(false),
    ProcSignature::dbms("dbms.listPools")
        .works_on_system()
        .described("List all memory pools, including sub pools, currently registered at this instance that are visible to the user.")
        .executable(false),
    ProcSignature::write("dbms.promoteReplicaDatabase")
        .works_on_system()
        .described("Convert a replica database to a standard database while keeping the data currently has. Optionally modify its topology.")
        .executable(false),
    ProcSignature::dbms("dbms.queryJmx")
        .works_on_system()
        .described("Query JMX management data by domain and name. For instance, use `*:*` to find all JMX beans.")
        .executable(false),
    ProcSignature::write("dbms.recreateDatabase")
        .works_on_system()
        .described("Recreates a database while keeping all RBAC settings. The procedure initiates a process, which when complete, will have synchronized and started all database instances within the cluster.")
        .executable(false),
    ProcSignature::dbms("dbms.reloadProcedures")
        .admin()
        .works_on_system()
        .described("Reload procedures from disk.")
        .executable(false),
    // CIP-113 procedure-signatures.feature [2]/[9]: the doc's own
    // literal worked example (an optional parameter defaulting to `null`).
    ProcSignature::dbms("dbms.routing.getRoutingTable")
        .works_on_system()
        .described(ROUTING_TABLE_DESCRIPTION)
        .args(ROUTING_TABLE_ARGS)
        .returns(ROUTING_TABLE_RETURNS)
        .executable(false),
    ProcSignature::dbms("dbms.scheduler.failedJobs")
        .admin()
        .works_on_system()
        .described("List failed job runs. There is a limit for amount of historical data.")
        .executable(false),
    ProcSignature::dbms("dbms.scheduler.groups")
        .admin()
        .works_on_system()
        .described("List the job groups that are active in the database internal job scheduler.")
        .executable(false),
    ProcSignature::dbms("dbms.scheduler.jobs")
        .admin()
        .works_on_system()
        .described("List all jobs that are active in the database internal job scheduler.")
        .executable(false),
    ProcSignature::dbms("dbms.security.clearAuthCache")
        .admin()
        .works_on_system()
        .described("Clears authentication and authorization cache.")
        .executable(false),
    ProcSignature::dbms("dbms.security.reloadTLS")
        .admin()
        .works_on_system()
        .described("Trigger the dynamic reloading of all TLS certificates and configuration.")
        .executable(false),
    ProcSignature::dbms("dbms.setConfigValue")
        .works_on_system()
        .described("Update a given setting value. Passing an empty value results in removing the configured value and falling back to the default value. Changes do not persist and are lost if the server is restarted. In a clustered environment, `dbms.setConfigValue` affects only the cluster member it is run against.")
        .admin()
        .writes()
        .args(&[
            ProcArgMeta::described("setting", "The name of the setting to update.", "STRING"),
            ProcArgMeta::described("value", "The new value of the setting.", "STRING"),
        ])
        .executable(false),
    ProcSignature::write("dbms.setDefaultAllocationNumbers")
        .admin()
        .works_on_system()
        .described("With this method you can set the default number of primaries, secondaries and property shard replicas.")
        .executable(false),
    ProcSignature::write("dbms.setDefaultDatabase")
        .admin()
        .works_on_system()
        .described("Change the default database to the provided value. The old default database must be stopped.")
        .executable(false),
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
    ProcSignature::read("dbms.showTopologyGraphConfig")
        .admin()
        .works_on_system()
        .described("With this method the configuration of the Topology Graph can be displayed.")
        .executable(false),
    ProcSignature::dbms("dbms.unquarantineDatabase")
        .admin()
        .works_on_system()
        .described("Lift quarantine from a database on a given server.")
        .executable(false),
    ProcSignature::dbms("fleetManagement.debugLogging")
        .admin()
        .works_on_system()
        .described("Enable debug or payload logging for Fleet Manager.")
        .executable(false),
    ProcSignature::dbms("fleetManagement.disable")
        .admin()
        .works_on_system()
        .described("Disable Fleet Manager")
        .executable(false),
    ProcSignature::dbms("fleetManagement.inspectToken")
        .admin()
        .works_on_system()
        .described("Inspect a Fleet Manager access token.")
        .executable(false),
    ProcSignature::dbms("fleetManagement.registerToken")
        .admin()
        .works_on_system()
        .described("Add a token for authenticating to Fleet Manager")
        .executable(false),
    ProcSignature::read("fleetManagement.reportedData")
        .works_on_system()
        .described("Generate documentation for the data structures used in Fleet Manager messages.")
        .executable(false),
    ProcSignature::dbms("fleetManagement.restart")
        .admin()
        .works_on_system()
        .described("Restart Fleet Manager.")
        .executable(false),
    ProcSignature::read("fleetManagement.status")
        .admin()
        .works_on_system()
        .described("Check the status of Fleet Manager.")
        .executable(false),
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
