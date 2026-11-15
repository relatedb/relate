import CoreLocation
import Testing
import Foundation
import Relate
@testable import SNOTELCore

@Suite("DatabaseService Tests")
struct DatabaseServiceTests {

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

    @Test("Curated demo queries execute and return tabular output", arguments: DemoQuery.examples)
    func demoQueries(query: DemoQuery) async throws {
        let svc = try await makeLoadedService()
        let result = try await svc.run(query.cypher)
        #expect(!result.columns.isEmpty)
        #expect(result.rows.allSatisfy { $0.count == result.columns.count })
        #expect(result.elapsedMs >= 0)
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
        #expect(loadEntries[0].parameters == ["rows"])

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
        #expect(entries[0].parameters == ["searchText"])
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
