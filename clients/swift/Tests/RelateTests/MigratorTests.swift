import Foundation
import Testing

@testable import Relate

@Suite struct MigratorTests {
    /// A library app's migrator at version 2: two schema statements, a step
    /// of statements and a step with a body.
    private func books() -> Migrator {
        var migrator = Migrator()
        migrator.schema = [
            "CREATE CONSTRAINT tag_name IF NOT EXISTS FOR (t:Tag) REQUIRE t.name IS UNIQUE",
            "CREATE INDEX book_status IF NOT EXISTS FOR (b:Book) ON (b.status)",
        ]
        migrator.register(version: 1, ["MATCH (b:Book) WHERE b.status IS NULL SET b.status = 'want'"])
        migrator.register(version: 2) { tx in
            try tx.run("CREATE (:Tag {name: \("Favorites")})")
        }
        return migrator
    }

    private func count(_ label: String, in conn: Connection) throws -> Int {
        try conn.run("MATCH (n:\(identifier: label)) RETURN count(n)").scalar(as: Int.self)
    }

    @Test func aFreshStoreRunsSchemaAndEveryStep() throws {
        let conn = try Database.inMemory().connect()
        try conn.query("CREATE (:Book {title: 'Dune'}), (:Book {title: 'Emma', status: 'read'})")
        let migrator = books()
        #expect(migrator.latestVersion == 2)
        #expect(try migrator.currentVersion(conn) == 0)
        #expect(try migrator.migrate(conn) == 2)
        #expect(try migrator.currentVersion(conn) == 2)
        #expect(try conn.query("MATCH (b:Book) RETURN b.status ORDER BY b.title").rows == [["want"], ["read"]])
        #expect(try conn.query("SHOW CONSTRAINTS").contains { $0["name"] == "tag_name" })
        #expect(try conn.query("MATCH (m:RelateMigration) RETURN m.key, m.version").rows == [["schema_version", 2]])
    }

    @Test func migratingAgainRepeatsNoStep() throws {
        let conn = try Database.inMemory().connect()
        #expect(try books().migrate(conn) == 2)
        // Schema is idempotent, and a second Favorites would violate the
        // constraint the first migration created.
        #expect(try books().migrate(conn) == 2)
        #expect(try count("Tag", in: conn) == 1)
    }

    @Test func aNewerBuildRunsOnlyTheNewSteps() throws {
        let conn = try Database.inMemory().connect()
        #expect(try books().migrate(conn) == 2)
        var next = books()
        next.register(version: 3, ["MATCH (t:Tag) SET t.color = 'yellow'"])
        next.register(version: 5, [])  // a version that only added to schema
        #expect(try next.migrate(conn) == 5)
        #expect(try conn.query("MATCH (t:Tag) RETURN t.color").rows == [["yellow"]])
        #expect(try count("Tag", in: conn) == 1)
    }

    @Test func aFailingStepLeavesItsWorkAndItsVersionUndone() throws {
        let conn = try Database.inMemory().connect()
        var migrator = Migrator()
        migrator.register(version: 1, ["CREATE (:Step {n: 1})"])
        migrator.register(version: 2) { tx in
            try tx.query("CREATE (:Step {n: 2})")
            try tx.query("RETURN 1/0")
        }
        migrator.register(version: 3, ["CREATE (:Step {n: 3})"])
        #expect(throws: RelateError.self) { try migrator.migrate(conn) }
        // Step 1 landed with its version; step 2 and its node did not; step 3 never ran.
        #expect(try migrator.currentVersion(conn) == 1)
        #expect(try conn.query("MATCH (s:Step) RETURN s.n").rows == [[1]])
    }

    @Test func aStoreFromANewerBuildIsRefused() throws {
        let conn = try Database.inMemory().connect()
        var newer = books()
        newer.register(version: 9, [])
        #expect(try newer.migrate(conn) == 9)
        let message = try misuseMessage { _ = try books().migrate(conn) }
        #expect(message.contains("version 9") && message.contains("build's 2"))
    }

    @Test func aMigratorWithNoStepsRefusesAVersionedStore() throws {
        let conn = try Database.inMemory().connect()
        #expect(try books().migrate(conn) == 2)
        let message = try misuseMessage { _ = try Migrator().migrate(conn) }
        #expect(message.contains("version 2") && message.contains("build's 0"))
    }

    @Test func aRefusedMigrationRunsNoSchemaStatement() throws {
        let conn = try Database.inMemory().connect()
        var newer = Migrator()
        newer.register(version: 9, [])
        #expect(try newer.migrate(conn) == 9)
        // An older build's schema statements do not touch a newer build's store.
        var older = Migrator()
        older.schema = ["CREATE (:AppliedByAnOlderBuild)"]
        _ = try misuseMessage { _ = try older.migrate(conn) }
        #expect(try count("AppliedByAnOlderBuild", in: conn) == 0)
    }

    @Test(arguments: [[2, 1], [1, 1], [0], [-1, 2]])
    func stepsRegisteredOutOfOrderAreRefusedBeforeAnythingRuns(versions: [Int]) throws {
        let conn = try Database.inMemory().connect()
        var migrator = Migrator()
        migrator.schema = ["CREATE (:SchemaRan)"]
        for version in versions { migrator.register(version: version, ["CREATE (:StepRan)"]) }
        let message = try misuseMessage { _ = try migrator.migrate(conn) }
        #expect(message.contains("ascending order"))
        #expect(try count("SchemaRan", in: conn) == 0 && count("StepRan", in: conn) == 0)
        #expect(try migrator.currentVersion(conn) == 0)
    }

    @Test func aVersionNodeWithoutAVersionIsABrokenRecordNotVersionZero() throws {
        let conn = try Database.inMemory().connect()
        try conn.query("CREATE (:RelateMigration {key: 'schema_version'})")
        var migrator = Migrator()
        migrator.register(version: 1, ["CREATE (:WouldRunAgain)"])
        let failure = try resultMismatch { _ = try migrator.migrate(conn) }
        #expect(failure.path == ["version"] && failure.expected == "Int" && failure.found == "null")
        #expect(try count("WouldRunAgain", in: conn) == 0)
    }

    @Test func severalVersionNodesAreABrokenRecordNotTheFirstOne() throws {
        let conn = try Database.inMemory().connect()
        try conn.query(
            "CREATE (:RelateMigration {key: 'schema_version', version: 1}), (:RelateMigration {key: 'schema_version', version: 3})")
        var migrator = Migrator()
        migrator.register(version: 2, ["CREATE (:WouldRunOnAGuess)"])
        let failure = try resultMismatch { _ = try migrator.migrate(conn) }
        #expect(failure.expected == "one version node" && failure.found == "several version nodes")
        #expect(try count("WouldRunOnAGuess", in: conn) == 0)
    }

    @Test func anAppsOwnVersionNodeIsHonoured() throws {
        let conn = try Database.inMemory().connect()
        // A label of the app's choosing: (:AppMeta {key: 'schema_version'}).version
        try conn.query("CREATE (:AppMeta {key: 'schema_version', version: 1}), (:AppMeta {key: 'other', version: 7})")
        var migrator = Migrator(versionLabel: "AppMeta")
        migrator.register(version: 1, ["CREATE (:ShouldNotRun)"])
        migrator.register(version: 2, ["CREATE (:Ran)"])
        #expect(try migrator.currentVersion(conn) == 1)
        #expect(try migrator.migrate(conn) == 2)
        #expect(try count("ShouldNotRun", in: conn) == 0 && count("Ran", in: conn) == 1)
        #expect(try conn.query("MATCH (m:AppMeta) RETURN m.key, m.version ORDER BY m.key").rows
            == [["other", 7], ["schema_version", 2]])
        #expect(try count("RelateMigration", in: conn) == 0)
    }

    @Test func aDatabaseMigratesOnItsWriter() async throws {
        let db = try Database.inMemory()
        try await db.write("CREATE (:Book {title: 'Dune'})")
        #expect(try await books().migrate(db) == 2)
        #expect(try await db.read("MATCH (b:Book) RETURN b.status").scalar() == "want")
    }

    @Test func aReadOnlyDatabaseRefusesToMigrateThroughEitherEntryPoint() async throws {
        try await withTemporaryStoreFile { url in
            do {
                let db = try Database(url: url)
                #expect(try await books().migrate(db) == 2)
                try await db.checkpointInBackground()
            }
            let reader = try Database(url: url, readOnly: true)
            let managed = try misuseMessage(
                of: try await #require(throws: RelateError.self) { try await books().migrate(reader) })
            // Nothing is pending for this store; the refusal does not depend on that.
            let own = try misuseMessage { _ = try books().migrate(reader.connect()) }
            #expect(managed == own && own.contains("migrate") && own.contains("read-only"))
        }
    }

    @Test func aReadOnlyDatabaseStillReportsItsVersion() async throws {
        try await withTemporaryStoreFile { url in
            do {
                let db = try Database(url: url)
                #expect(try await books().migrate(db) == 2)
                try await db.checkpointInBackground()
            }
            #expect(try books().currentVersion(Database(url: url, readOnly: true).connect()) == 2)
        }
    }
}
