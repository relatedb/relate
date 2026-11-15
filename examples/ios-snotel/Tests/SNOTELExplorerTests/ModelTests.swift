import Testing
import Relate
@testable import SNOTELCore

@Suite("Model Tests")
struct ModelTests {
    /// Decodes a model from one result row given as its columns, the way
    /// `QueryResult.decode` does for each record.
    private func decode<T: Decodable>(_ type: T.Type, _ row: [String: CypherValue]) throws -> T {
        try CypherValueDecoder().decode(T.self, from: .map(row))
    }

    @Test("SWELevel classification")
    func sweLevel() {
        #expect(SWELevel.from(swe: nil) == .noData)
        #expect(SWELevel.from(swe: 0) == .low)
        #expect(SWELevel.from(swe: 5) == .low)
        #expect(SWELevel.from(swe: 9.9) == .low)
        #expect(SWELevel.from(swe: 10) == .moderate)
        #expect(SWELevel.from(swe: 15) == .moderate)
        #expect(SWELevel.from(swe: 20) == .high)
        #expect(SWELevel.from(swe: 34.9) == .high)
        #expect(SWELevel.from(swe: 35) == .veryHigh)
        #expect(SWELevel.from(swe: 50) == .veryHigh)
    }

    @Test("A station decodes from its projection's row")
    func stationFromRow() throws {
        let row: [String: CypherValue] = [
            "id": .int(302),
            "name": .string("Aneroid Lake"),
            "state": .string("OR"),
            "latitude": .float(45.0833),
            "longitude": .float(-117.15),
            "elevation": .int(7500),
            "sweInches": .float(28.5),
        ]
        let station = try decode(Station.self, row)
        #expect(station.id == 302)
        #expect(station.name == "Aneroid Lake")
        #expect(station.elevation == 7500)
        #expect(station.sweInches == 28.5)
        #expect(station.sweLevel == .high)
    }

    @Test("A station without a reading has no SWE")
    func stationMissingSWE() throws {
        let row: [String: CypherValue] = [
            "id": .int(1),
            "name": .string("Test"),
            "state": .string("XX"),
            "latitude": .float(40.0),
            "longitude": .float(-110.0),
            "elevation": .int(5000),
        ]
        let station = try decode(Station.self, row)
        #expect(station.sweInches == nil)
        #expect(station.sweLevel == .noData)
    }

    @Test("A station row missing a guaranteed column is refused")
    func stationMissingElevation() {
        let row: [String: CypherValue] = [
            "id": .int(1),
            "name": .string("Test"),
            "state": .string("XX"),
            "latitude": .float(40.0),
            "longitude": .float(-110.0),
        ]
        #expect(throws: RelateError.self) { try decode(Station.self, row) }
    }

    @Test("A basin decodes from its query's row, identified by its HUC2 code")
    func basinFromRow() throws {
        let row: [String: CypherValue] = [
            "name": .string("Pacific Northwest"),
            "huc2": .string("17"),
            "geoJSON": .string("{\"type\":\"Polygon\"}"),
            "avgSWE": .float(22.5),
            "stationCount": .int(12),
        ]
        let basin = try decode(Basin.self, row)
        #expect(basin.name == "Pacific Northwest" && basin.id == "17")
        #expect(basin.avgSWE == 22.5)
        #expect(basin.stationCount == 12)
        #expect(basin.geoJSON.contains("Polygon"))
    }

    @Test("A basin without readings has no average")
    func basinWithoutReadings() throws {
        let basin = try decode(Basin.self, [
            "name": .string("Dry"), "huc2": .string("15"), "geoJSON": .string("{}"),
            "avgSWE": .null, "stationCount": .int(0),
        ])
        #expect(basin.avgSWE == nil)
        #expect(basin.stationCount == 0)
    }

    @Test("The snowpack ranking is the basins with readings, highest mean first")
    func ranking() {
        func basin(_ huc2: String, _ avgSWE: Double?) -> Basin {
            Basin(name: "Basin \(huc2)", huc2: huc2, geoJSON: "{}", avgSWE: avgSWE, stationCount: 1)
        }
        let ranked = [basin("15", 4.5), basin("16", nil), basin("17", 22.5)].bySWE
        #expect(ranked.map(\.id) == ["17", "15"] && ranked.map(\.swe) == [22.5, 4.5])
    }

    @Test("QueryLogEntry exposes a failure's message")
    func logEntryFailure() {
        let entry = QueryLogEntry(cypher: "X", parameters: [], durationMilliseconds: 0,
                                  queryType: nil, rowCount: 0, outcome: .failure("boom"))
        #expect(entry.failureMessage == "boom")
        #expect(!entry.succeeded)
    }

    @Test("A basin row without a name is refused")
    func basinMissingName() {
        #expect(throws: RelateError.self) { try decode(Basin.self, ["huc2": .string("17")]) }
    }

    @Test("The legend lists every level, no-data last")
    func legendOrder() {
        #expect(SWELevel.allCases.last == .noData)
        #expect(SWELevel.noData.legendLabel == "No data")
    }
}
