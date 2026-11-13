import CoreLocation
import Testing
import Foundation
import Relate
@testable import SNOTELCore

@Suite("DatabaseService Tests")
struct DatabaseServiceTests {

    private func makeService() async throws -> DatabaseService {
        let svc = try await DatabaseService()
        try await svc.load(
            stations: DataLoader.parseStationsCSV(sampleStationsCSV),
            readings: DataLoader.parseReadingsCSV(sampleReadingsCSV),
            basins: DataLoader.parseBasinsGeoJSON(sampleBasinsJSON)
        )
        return svc
    }

    /// What a view showing `statement` is given: the app reads stations
    /// only this way, through the store's database.
    private func shownStations(_ statement: Statement, in svc: DatabaseService) async throws -> [Station] {
        try await svc.database.read { tx in try tx.run(statement).decode(Station.self) }
    }

    private func shownStations(_ filter: StationFilter, in svc: DatabaseService) async throws -> [Station] {
        try await shownStations(try SNOTELQueries.stations(filter), in: svc)
    }

    @Test("A durable store reopens loaded and skips the bundled load")
    func durableStoreReopens() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("snotel-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        do {
            let svc = try await DatabaseService(path: dir.path)
            #expect(try await !svc.isLoaded())
            try await svc.loadBundledDataIfNeeded()
            #expect(try await svc.isLoaded())
        }
        let reopened = try await DatabaseService(path: dir.path)
        #expect(try await reopened.isLoaded())
        try await reopened.loadBundledDataIfNeeded()
        #expect(try await shownStations(.all, in: reopened).count == 50)
    }

    @Test("A failed load commits nothing and does not read as loaded")
    func failedLoadIsAtomic() async throws {
        let svc = try await DatabaseService()
        // NaN is a valid FLOAT parameter under the typed ABI, so the trigger
        // is a coordinate point() refuses: an out-of-range latitude appended
        // after the valid stations fails the stations statement once earlier
        // rows were created, inside the load transaction.
        let badStation = StationCSVRow(id: 999_999, name: "Nowhere", state: "XX",
                                       latitude: 200, longitude: 0, elevationFt: 0)
        let stations = try DataLoader.parseStationsCSV(sampleStationsCSV) + [badStation]
        await #expect(throws: (any Error).self) {
            try await svc.load(stations: stations, readings: [], basins: [])
        }
        #expect(try await !svc.isLoaded())
        #expect(try await shownStations(.all, in: svc).isEmpty)
    }

    @Test("Load stations and query all")
    func loadAndQueryStations() async throws {
        let svc = try await makeService()
        let all = try await shownStations(.all, in: svc)
        #expect(all.count == 3)
        #expect(all.contains { $0.name == "Bogus Basin" })
    }

    @Test("Query stations in basin via IN_BASIN")
    func stationsInBasin() async throws {
        let svc = try await makeService()
        let pnw = try await shownStations(.basin("Pacific Northwest"), in: svc)
        // Blewett Pass (WA) and Bumping Ridge (WA) are in PNW; Bogus Basin (ID) is not
        let names = pnw.map(\.name)
        #expect(names.contains("Blewett Pass"))
        #expect(names.contains("Bumping Ridge"))
        #expect(!names.contains("Bogus Basin"))
    }

    @Test("Nearby stations using point.distance")
    func nearbyStations() async throws {
        let svc = try await makeService()
        // Search near Blewett Pass (47.53, -120.62), 200km radius
        let nearby = try await shownStations(
            .nearby(CLLocationCoordinate2D(latitude: 47.53, longitude: -120.62), radiusKm: 200), in: svc)
        #expect(nearby.count >= 1)
        #expect(nearby.first?.name == "Blewett Pass") // closest
        // Verify distance is populated
        #expect(nearby.first?.distanceMeters != nil)
    }

    @Test("Bounding box query")
    func boundingBoxQuery() async throws {
        let svc = try await makeService()
        // Box around Washington state
        let inBox = try await shownStations(
            try SNOTELQueries.stations(
                southWest: CLLocationCoordinate2D(latitude: 46.0, longitude: -122.0),
                northEast: CLLocationCoordinate2D(latitude: 49.0, longitude: -119.0)),
            in: svc)
        let names = inBox.map(\.name)
        #expect(names.contains("Blewett Pass"))
        #expect(names.contains("Bumping Ridge"))
        #expect(!names.contains("Bogus Basin"))
    }

    @Test("Basins carry geometry and snowpack aggregates")
    func basinAggregates() async throws {
        let svc = try await makeService()
        let basins = try await svc.database.read(SNOTELQueries.basins).decode(Basin.self)
        #expect(basins.count == 2)
        let pnw = try #require(basins.first { $0.name == "Pacific Northwest" })
        #expect(pnw.avgSWE != nil)
        #expect(pnw.stationCount == 2)
        #expect(pnw.geoJSON.contains("Polygon"))
    }

    @Test("Query parameters preserve station names with punctuation")
    func parameterizedStationValues() async throws {
        let svc = try await DatabaseService()
        try await svc.load(stations: [
            StationCSVRow(
                id: 100,
                name: "O'Brien's \\ Snow Course",
                state: "CO",
                latitude: 40,
                longitude: -105,
                elevationFt: 9_000
            ),
        ], readings: [], basins: [])
        #expect(try await shownStations(.all, in: svc).first?.name == "O'Brien's \\ Snow Course")
    }

    @Test("Spatial containment can be materialized as graph relationships")
    func basinMembershipRelationships() async throws {
        let svc = try await makeService()
        let stats = try await svc.database.read(SNOTELQueries.stats)
        #expect(stats.stations == 3)
        #expect(stats.readings == 3)
        #expect(stats.basins == 2)
        #expect(stats.nodes == 8)
        #expect(stats.basinMemberships == 3)
        #expect(stats.relationships == 6)
    }

    @Test("Station search matches names and state abbreviations")
    func stationSearch() async throws {
        let svc = try await makeService()
        #expect(try await shownStations(.search("Blewett"), in: svc).map(\.name) == ["Blewett Pass"])
        #expect(try await shownStations(.search("WA"), in: svc).count == 2)
    }

    @Test("Curated demo queries execute and return tabular output", arguments: DemoQuery.examples)
    func demoQueries(query: DemoQuery) async throws {
        let svc = try await makeService()
        let result = try await svc.run(query.cypher)
        #expect(!result.columns.isEmpty)
        #expect(result.rows.allSatisfy { $0.count == result.columns.count })
        #expect(result.elapsedMs >= 0)
    }

    @Test("A nearby search off the globe is refused when its statement is built")
    func nearbyOffTheGlobe() {
        #expect(throws: RelateError.self) {
            try SNOTELQueries.stations(.nearby(CLLocationCoordinate2D(latitude: 200, longitude: 0), radiusKm: 10))
        }
    }

    @Test("A live station query follows a workbench write")
    func liveQueryFollowsAWrite() async throws {
        let svc = try await makeService()
        // What `@LiveQuery` observes for the map: the value now, then again
        // after a commit that changes it.
        let everyStation = try SNOTELQueries.stations(.all)
        var shown = svc.database.values { tx in
            try tx.run(everyStation).decode(Station.self).map(\.name)
        }.makeAsyncIterator()
        #expect(try await shown.next()?.count == 3)
        _ = try await svc.run(
            "CREATE (:Station {id: 1, name: 'Added In The Workbench', state: 'CO', latitude: 40.0, longitude: -105.0, elevation: 9000})")
        #expect(try await shown.next()?.contains("Added In The Workbench") == true)
    }

    @Test("The query log records every statement, with parameter names and failures")
    func queryLogging() async throws {
        let svc = try await DatabaseService()
        var log = svc.queryLog.makeAsyncIterator()
        try await svc.load(stations: [
            StationCSVRow(
                id: 1,
                name: "Logger Station",
                state: "CO",
                latitude: 40,
                longitude: -105,
                elevationFt: 8_000
            ),
        ], readings: [], basins: [])
        // The load's entries: the four UNWINDs, then the marker. (The
        // index is schema, created when the store opens.)
        let loadEntries = await next(5, from: &log)
        #expect(loadEntries.count == 5)
        #expect(loadEntries[0].operation == "UNWIND")
        #expect(loadEntries[0].queryType == .write)
        // A trace carries the names of the parameters bound, not their values.
        #expect(loadEntries[0].parameters == "$rows")

        _ = try await shownStations(.search("Logger"), in: svc)

        do {
            _ = try await svc.run("THIS IS NOT CYPHER")
            Issue.record("Invalid Cypher unexpectedly succeeded")
        } catch {
            // The failure is still logged.
        }

        let entries = await next(2, from: &log)
        #expect(entries.count == 2)
        #expect(entries[0].queryType == .read)
        #expect(entries[0].rowCount == 1)
        #expect(entries[0].parameters == "$searchText")
        #expect(entries[1].succeeded == false)
    }
}

/// The next `count` query-log entries, in execution order.
private func next(
    _ count: Int, from log: inout AsyncStream<QueryLogEntry>.Iterator
) async -> [QueryLogEntry] {
    var entries: [QueryLogEntry] = []
    while entries.count < count, let entry = await log.next() {
        entries.append(entry)
    }
    return entries
}

// MARK: - Sample Test Data

private let sampleStationsCSV = """
id,name,state,latitude,longitude,elevation_ft
515,Bogus Basin,ID,43.7667,-116.1,6040
526,Blewett Pass,WA,47.5333,-120.6167,4240
978,Bumping Ridge,WA,46.8833,-121.3,4600
"""

private let sampleReadingsCSV = """
station_id,date,swe_inches,snow_depth_inches,temp_avg_f
515,2024-04-01,8.1,22.0,38
526,2024-04-01,14.2,38.0,35
978,2024-04-01,18.7,48.0,33
"""

private let sampleBasinsJSON: Data = """
{
  "type": "FeatureCollection",
  "features": [
    {
      "type": "Feature",
      "properties": { "name": "Pacific Northwest", "huc2": "17" },
      "geometry": {
        "type": "Polygon",
        "coordinates": [[
          [-125.0, 42.0], [-125.0, 49.0], [-117.0, 49.0],
          [-117.0, 46.5], [-116.0, 46.0], [-116.5, 44.5],
          [-117.5, 44.0], [-117.5, 42.0], [-125.0, 42.0]
        ]]
      }
    },
    {
      "type": "Feature",
      "properties": { "name": "Upper Snake", "huc2": "17-04" },
      "geometry": {
        "type": "Polygon",
        "coordinates": [[
          [-117.5, 42.0], [-116.5, 44.5], [-114.5, 44.5],
          [-111.0, 44.5], [-111.0, 42.0], [-114.5, 42.0],
          [-117.5, 42.0]
        ]]
      }
    }
  ]
}
""".data(using: .utf8)!
