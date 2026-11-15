import CoreLocation
import Testing
import Foundation
import Relate
@testable import SNOTELCore

/// The app's reads (`SNOTELQueries`), run against a loaded store the way a
/// view's live query runs them.
@Suite("SNOTELQueries Tests")
struct SNOTELQueriesTests {

    @Test("Load stations and query all")
    func loadAndQueryStations() async throws {
        let svc = try await makeLoadedService()
        let all = try await shownStations(.all, in: svc)
        #expect(all.count == 3)
        #expect(all.contains { $0.name == "Bogus Basin" })
    }

    @Test("Query stations in basin via IN_BASIN")
    func stationsInBasin() async throws {
        let svc = try await makeLoadedService()
        let pnw = try await shownStations(.basin("17"), in: svc)
        // Blewett Pass (WA) and Bumping Ridge (WA) are in the Pacific
        // Northwest (HUC2 17); Bogus Basin (ID) is not
        let names = pnw.map(\.name)
        #expect(names.contains("Blewett Pass"))
        #expect(names.contains("Bumping Ridge"))
        #expect(!names.contains("Bogus Basin"))
    }

    @Test("Nearby stations using point.distance")
    func nearbyStations() async throws {
        let svc = try await makeLoadedService()
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
        let svc = try await makeLoadedService()
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
        let svc = try await makeLoadedService()
        let basins = try await svc.database.read(SNOTELQueries.basins).decode(Basin.self)
        #expect(basins.count == 2)
        let pnw = try #require(basins.first { $0.name == "Pacific Northwest" })
        #expect(pnw.avgSWE != nil)
        #expect(pnw.stationCount == 2)
        #expect(pnw.outline.outer.count == 9)
    }

    @Test("The insights read counts the graph and lists its basins in one snapshot")
    func insightsSnapshot() async throws {
        let svc = try await makeLoadedService()
        let insights = try await svc.database.read(SNOTELQueries.insights)
        #expect(insights.stats == DatabaseStats(
            stations: 3, readings: 3, basins: 2, relationships: 6, basinMemberships: 3))
        #expect(insights.basins.map(\.name) == ["Pacific Northwest", "Upper Snake"])
    }

    @Test("Station search matches a name or a state abbreviation", arguments: [
        ("Blewett", ["Blewett Pass"]),
        ("WA", ["Blewett Pass", "Bumping Ridge"]),
    ])
    func stationSearch(text: String, found: [String]) async throws {
        let svc = try await makeLoadedService()
        #expect(try await shownStations(.search(text), in: svc).map(\.name) == found)
    }

    @Test("A nearby search off the globe is refused when its statement is built")
    func nearbyOffTheGlobe() {
        #expect(throws: RelateError.self) {
            try SNOTELQueries.stations(.nearby(CLLocationCoordinate2D(latitude: 200, longitude: 0), radiusKm: 10))
        }
    }

    @Test("A live station query follows a workbench write")
    func liveQueryFollowsAWrite() async throws {
        let svc = try await makeLoadedService()
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
}
