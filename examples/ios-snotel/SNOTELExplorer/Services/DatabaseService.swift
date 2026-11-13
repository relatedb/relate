import Foundation
import Relate

/// The app's store: opening it, loading the bundled dataset on first
/// launch, checkpointing, and running the workbench's statements.
///
/// The app's views do not go through this class to read. Each declares the
/// query it shows with `@LiveQuery` (see `SNOTELQueries`) against
/// `database`, which the root view puts in the environment; Relate runs
/// those reads on connections it manages and runs them again when a write
/// commits, so no view asks for a refresh.
///
/// The service holds no connection and no mutable state, which is why it
/// is a `Sendable` class rather than an actor.
///
/// Every statement run on the open store — a view's live query, the load,
/// a workbench statement — is reported by the database's trace handler and
/// yielded, in execution order, to `queryLog`. (The schema statements of
/// `init` run before the handler is set and are not in the log.) A trace
/// carries parameter names, not values, so the log is safe to show
/// whatever the data is.
final class DatabaseService: Sendable {
    /// The open database: what `@LiveQuery` reads once it is in the
    /// environment (`.relateDatabase(_:)`).
    let database: Database
    private let logContinuation: AsyncStream<QueryLogEntry>.Continuation
    /// One entry per executed statement, successes and failures alike.
    let queryLog: AsyncStream<QueryLogEntry>

    /// `":memory:"` for a throwaway store, or a directory for a durable one
    /// (WAL + checkpoints).
    ///
    /// Opening the store brings its schema up to date: the index is
    /// declared once, in `migrator`, and exists before any load or query.
    /// The initializer is `async` so that the open and the migration run
    /// off the caller's actor — the `MainActor`, at launch.
    init(path: String = ":memory:") async throws {
        database = try Database(path: path)
        try await Self.migrator.migrate(database)
        let (queryLog, logContinuation) = AsyncStream<QueryLogEntry>.makeStream()
        self.queryLog = queryLog
        self.logContinuation = logContinuation
        database.setTraceHandler { trace in logContinuation.yield(QueryLogEntry(trace)) }
    }

    /// The store's schema. `schema` statements run at every open, so each
    /// is idempotent; a later release that reshapes existing data registers
    /// a versioned step here.
    private static let migrator: Migrator = {
        var migrator = Migrator()
        migrator.schema = [
            "CREATE POINT INDEX station_location IF NOT EXISTS FOR (s:Station) ON (s.location)"
        ]
        return migrator
    }()

    deinit {
        logContinuation.finish()
    }

    /// The app's durable store: `SNOTEL.relate` in Application Support.
    /// Application Support (not Documents) is the place for app-managed data
    /// the user never browses; `completeUntilFirstUserAuthentication` keeps
    /// the store writable while the device is locked after first unlock.
    static func persistentStoreURL() throws -> URL {
        let url = try Database.defaultStoreURL(named: "SNOTEL.relate")
        #if os(iOS)
        try FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true,
            attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
        #else
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        #endif
        return url
    }

    /// Whether the store already holds the bundled dataset: its completion
    /// marker commits in the same transaction as the data, so a load that
    /// failed part-way never reads as loaded.
    func isLoaded() async throws -> Bool {
        try await database.read("MATCH (d:Dataset {name: 'snotel'}) RETURN count(d)").scalar(as: Int.self) > 0
    }

    // MARK: - Data Loading

    /// First-launch setup: reads and parses the bundled dataset (never on
    /// the `MainActor`), loads it, and checkpoints so the next launch opens
    /// from a snapshot instead of replaying the WAL. A store that is
    /// already loaded is left as is.
    func loadBundledDataIfNeeded(bundle: Bundle? = nil) async throws {
        guard try await !isLoaded() else { return }
        try await load(
            stations: DataLoader.loadStationsCSV(from: bundle),
            readings: DataLoader.loadReadingsCSV(from: bundle),
            basins: DataLoader.loadBasinsGeoJSON(from: bundle)
        )
        try await database.checkpointInBackground()
    }

    /// Loads one dataset all-or-nothing: every dataset's parameterized UNWIND
    /// (one statement per dataset, not one per row), the basin memberships and
    /// the completion marker `isLoaded()` probes commit in one transaction.
    func load(stations: [StationCSVRow], readings: [ReadingCSVRow], basins: [BasinFeature]) async throws {
        try await database.write(Self.loadSteps(stations: stations, readings: readings, basins: basins))
    }

    /// The load, as the statements it runs, in order. Each dataset's rows
    /// are `Encodable`: the array binds as one list-of-maps parameter whose
    /// keys are the rows' Swift property names.
    private static func loadSteps(
        stations: [StationCSVRow], readings: [ReadingCSVRow], basins: [BasinFeature]
    ) throws -> [Statement] {
        [
            Statement(
                """
                UNWIND $rows AS row
                CREATE (:Station {
                    id: row.id, name: row.name, state: row.state,
                    latitude: row.latitude, longitude: row.longitude,
                    elevation: row.elevationFt,
                    location: point({latitude: row.latitude, longitude: row.longitude})
                })
                """,
                ["rows": try CypherValue(encoding: stations)]),
            Statement(
                """
                UNWIND $rows AS row
                MATCH (s:Station {id: row.stationId})
                CREATE (s)-[:HAS_READING]->(:Reading {
                    date: row.date,
                    swe_inches: row.sweInches,
                    snow_depth_inches: row.snowDepthInches,
                    temp_avg_f: row.tempAvgF
                })
                """,
                ["rows": try CypherValue(encoding: readings)]),
            Statement(
                """
                UNWIND $rows AS row
                CREATE (:Basin {name: row.name, huc2: row.huc2, geojson: row.geoJSON})
                """,
                ["rows": try CypherValue(encoding: basins.map(BasinRow.init))]),
            Statement(
                """
                UNWIND $memberships AS m
                MATCH (s:Station {id: m.station}), (b:Basin {name: m.basin})
                CREATE (s)-[:IN_BASIN]->(b)
                """,
                ["memberships": try CypherValue(encoding: memberships(of: stations, in: basins))]),
            Statement("CREATE (:Dataset {name: 'snotel'})"),
        ]
    }

    /// Station-in-basin containment, materialized as `IN_BASIN`
    /// relationships so basin queries are plain graph traversals.
    ///
    /// The containment test runs in Swift because the engine's geospatial
    /// surface is the point — geometry tests happen in Swift over the stored
    /// basin geometry. The native point functions (`point.distance`,
    /// `point.withinBBox`) stay in Cypher, in `SNOTELQueries`.
    private static func memberships(
        of stations: [StationCSVRow], in basins: [BasinFeature]
    ) -> [Membership] {
        stations.flatMap { station in
            basins
                .filter { $0.contains(latitude: station.latitude, longitude: station.longitude) }
                .map { Membership(station: station.id, basin: $0.name) }
        }
    }

    /// What the basin load stores of a feature (its rings stay in Swift).
    private struct BasinRow: Encodable {
        let name: String
        let huc2: String
        let geoJSON: String

        init(_ feature: BasinFeature) {
            name = feature.name
            huc2 = feature.huc2
            geoJSON = feature.geoJSON
        }
    }

    /// One station-in-basin pair of the membership load.
    private struct Membership: Encodable {
        let station: Int
        let basin: String
    }

    // MARK: - Workbench

    /// Runs a workbench statement; rendering the result is the view's job.
    /// The workbench accepts any Cypher, writes included, so it runs as a
    /// write transaction; when it commits a write, the live queries on
    /// screen read again.
    func run(_ cypher: String) async throws -> QueryResult {
        try await database.write { tx in try tx.query(cypher) }
    }
}
