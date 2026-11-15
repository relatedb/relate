// MARK: - Schema migrations

/// Brings a store's schema to the version an app build expects.
///
/// Two kinds of work, in this order:
///
/// 1. ``schema`` — statements run on every launch, so they must be
///    idempotent (`CREATE INDEX … IF NOT EXISTS`). This is where indexes and
///    constraints live.
/// 2. Versioned steps — each registered for one version, run once, in
///    ascending order, for every version above the one the store records.
///    A step and the record of its version commit in one transaction, so a
///    step either happened and is recorded, or did neither.
///
/// ```swift
/// var migrator = Migrator()
/// migrator.schema = ["CREATE CONSTRAINT tag_name IF NOT EXISTS FOR (t:Tag) REQUIRE t.name IS UNIQUE"]
/// migrator.register(version: 1, ["MATCH (b:Book) WHERE b.status IS NULL SET b.status = 'want'"])
/// try await migrator.migrate(db)
/// ```
///
/// The version is the `version` property of one node,
/// `(:RelateMigration {key: 'schema_version'})`, in the namespace of the
/// connection the migration runs on (the default namespace for
/// `migrate(_ database:)`); ``versionLabel`` changes the label.
///
/// Run one migration at a time. `migrate(_ database:)` does, on the
/// database's single writer; two connections migrating one store at once
/// can each run the same step.
public struct Migrator: Sendable {
    /// The label of the node that holds the schema version. Change it when
    /// the default would collide with a label of the app's own.
    public var versionLabel: String
    /// Statements run on every migration, before the versioned steps. Each
    /// runs on its own, so each must be safe to repeat.
    public var schema: [Statement] = []
    private var steps: [(version: Int, body: @Sendable (Transaction) throws -> Void)] = []

    public init(versionLabel: String = "RelateMigration") {
        self.versionLabel = versionLabel
    }

    /// The highest registered version: what `migrate` leaves the store at. Zero when no step is registered.
    public var latestVersion: Int { steps.map(\.version).max() ?? 0 }

    /// Registers the step that brings a store to `version`. Versions are
    /// positive and registered in ascending order, each once; a migrator
    /// whose steps are not is refused by `migrate`, before anything runs.
    public mutating func register(
        version: Int, _ body: @escaping @Sendable (Transaction) throws -> Void
    ) {
        steps.append((version, body))
    }

    /// Registers a step made of statements. Pass an empty list for a
    /// version that only added to ``schema``.
    public mutating func register(version: Int, _ statements: [Statement]) {
        register(version: version) { tx in try tx.run(statements) }
    }

    /// Refuses steps that were not registered with positive, ascending
    /// versions, each once: run in the order given they would skip or
    /// repeat work on a store part-way through them.
    private func checkStepOrder() throws(RelateError) {
        var previous = 0
        for step in steps {
            guard step.version > previous else {
                throw RelateError.misuse(
                    "migration versions are positive and registered in ascending order, each once: "
                        + "\(step.version) was registered after \(previous)")
            }
            previous = step.version
        }
    }

    /// The pattern that addresses the version node, bound as `m`: the one
    /// place the node's shape is spelled.
    private var versionNode: Statement {
        "(m:\(identifier: versionLabel) {key: 'schema_version'})"
    }

    /// The version the store records; zero for a store never migrated (no
    /// version node). A broken record — several version nodes, or one
    /// without an integer version — is reported as
    /// ``RelateError/unexpectedResult(_:)`` rather than read as zero or as
    /// whichever node came first, either of which would run steps again.
    public func currentVersion(_ connection: Connection) throws(RelateError) -> Int {
        let found = try connection.run("MATCH \(versionNode) RETURN m.version AS version")
        guard found.count <= 1 else {
            throw shapeMismatch(expected: "one version node", found: "several version nodes")
        }
        guard let node = found.first else { return 0 }
        return try node.get("version", as: Int.self)
    }

    /// Runs ``schema``, then every registered step above the store's
    /// version, and returns the version the store is at afterwards.
    ///
    /// A store already at a version above ``latestVersion`` was written by
    /// a newer build; that is refused with ``RelateError/misuse(_:)`` before
    /// anything runs, ``schema`` included, rather than run against a schema
    /// this build does not know. So are a migrator whose steps were
    /// registered out of order, and a connection of a database opened
    /// read-only.
    @discardableResult
    public func migrate(_ connection: Connection) throws -> Int {
        try connection.database.requireWritable(for: "migrate")
        return try run(on: connection)
    }

    /// Migrates through the database's writer connection, off the caller's
    /// thread.
    @discardableResult
    public func migrate(_ database: Database) async throws -> Int {
        // The hop onto the writer refuses a read-only database itself.
        try await database.onWriter("migrate") { connection in try run(on: connection) }
    }

    /// The migration itself, on a connection already known to be writable.
    private func run(on connection: Connection) throws -> Int {
        try checkStepOrder()
        let version = try currentVersion(connection)
        guard version <= latestVersion else {
            throw RelateError.misuse(
                "the store is at schema version \(version), newer than this build's \(latestVersion)")
        }
        for statement in schema { try connection.run(statement) }
        let pending = steps.filter { $0.version > version }
        for step in pending {
            try connection.transaction { tx in
                try step.body(tx)
                try tx.run("MERGE \(versionNode) SET m.version = \(step.version)")
            }
        }
        return pending.last?.version ?? version
    }
}
