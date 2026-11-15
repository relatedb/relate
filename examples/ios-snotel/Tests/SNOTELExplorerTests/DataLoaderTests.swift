import Testing
import Foundation
@testable import SNOTELCore

@Suite("DataLoader Tests")
struct DataLoaderTests {

    @Test("Parse stations CSV")
    func parseStationsCSV() throws {
        let csv = """
        id,name,state,latitude,longitude,elevation_ft
        302,Aneroid Lake,OR,45.0833,-117.15,7500
        526,Blewett Pass,WA,47.5333,-120.6167,4240
        """
        let stations = try DataLoader.parseStationsCSV(csv)
        #expect(stations.count == 2)
        #expect(stations[0].id == 302)
        #expect(stations[0].name == "Aneroid Lake")
        #expect(stations[0].state == "OR")
        #expect(stations[0].latitude == 45.0833)
        #expect(stations[0].longitude == -117.15)
        #expect(stations[0].elevationFt == 7500)
    }

    @Test("Parse readings CSV")
    func parseReadingsCSV() throws {
        let csv = """
        station_id,date,swe_inches,snow_depth_inches,temp_avg_f
        302,2024-04-01,28.5,72.0,32
        526,2024-04-01,14.2,38.0,35
        """
        let readings = try DataLoader.parseReadingsCSV(csv)
        #expect(readings.count == 2)
        #expect(readings[0].stationId == 302)
        #expect(readings[0].sweInches == 28.5)
        #expect(readings[0].snowDepthInches == 72.0)
        #expect(readings[0].tempAvgF == 32)
    }

    @Test("Parse basins GeoJSON")
    func parseBasinsGeoJSON() throws {
        let json = """
        {
          "type": "FeatureCollection",
          "features": [{
            "type": "Feature",
            "properties": { "name": "Test Basin", "huc2": "99" },
            "geometry": {
              "type": "Polygon",
              "coordinates": [[[-120.0, 40.0], [-120.0, 42.0], [-118.0, 42.0], [-118.0, 40.0], [-120.0, 40.0]]]
            }
          }]
        }
        """.data(using: .utf8)!
        let basins = try DataLoader.parseBasinsGeoJSON(json)
        #expect(basins.count == 1)
        #expect(basins[0].name == "Test Basin")
        #expect(basins[0].huc2 == "99")
        #expect(basins[0].outline == BasinOutline(
            outer: ring((-120, 40), (-120, 42), (-118, 42), (-118, 40), (-120, 40))))
    }

    @Test("Basin containment excludes holes")
    func basinContainmentWithHole() {
        let outline = BasinOutline(
            outer: squareOutline.outer, holes: [ring((4, 4), (6, 4), (6, 6), (4, 6), (4, 4))])
        #expect(outline.contains(latitude: 2, longitude: 2))
        #expect(!outline.contains(latitude: 5, longitude: 5))
        #expect(!outline.contains(latitude: 11, longitude: 5))
    }

    @Test("An outline survives being stored as GeoJSON text and parsed back")
    func outlineRoundTrip() throws {
        let outline = BasinOutline(
            outer: squareOutline.outer, holes: [ring((4, 4), (6, 4), (6, 6), (4, 6), (4, 4))])
        #expect(try BasinOutline(geoJSON: outline.geoJSON()) == outline)
    }

    @Test("Empty CSV returns empty array")
    func emptyCSV() throws {
        let stations = try DataLoader.parseStationsCSV("")
        #expect(stations.isEmpty)

        let headerOnly = try DataLoader.parseStationsCSV("id,name,state,latitude,longitude,elevation_ft")
        #expect(headerOnly.isEmpty)
    }

    @Test("A malformed CSV row throws naming the row")
    func malformedCSV() {
        let csv = """
        id,name,state,latitude,longitude,elevation_ft
        not_a_number,Bad,XX,0,0,0
        302,Aneroid Lake,OR,45.0833,-117.15,7500
        """
        do {
            _ = try DataLoader.parseStationsCSV(csv)
            Issue.record("Expected a malformed-row error")
        } catch {
            #expect(error.localizedDescription.contains("not_a_number,Bad"))
        }
    }

    @Test("A malformed basins document throws instead of yielding no basins")
    func malformedBasins() {
        #expect(throws: DataLoaderError.self) { try DataLoader.parseBasinsGeoJSON(Data("[]".utf8)) }
        let unnamed = Data(#"{"features": [{"properties": {}, "geometry": {}}]}"#.utf8)
        #expect(throws: DataLoaderError.self) { try DataLoader.parseBasinsGeoJSON(unnamed) }
    }

    @Test("Quoted CSV fields support commas and escaped quotes")
    func quotedCSV() throws {
        let csv = """
        id,name,state,latitude,longitude,elevation_ft
        100,"Lake, Upper",CO,40.0,-105.0,9000
        101,"The ""Snow"" Course",UT,41.0,-111.0,8000
        """
        let stations = try DataLoader.parseStationsCSV(csv)
        #expect(stations.count == 2)
        #expect(stations[0].name == "Lake, Upper")
        #expect(stations[1].name == "The \"Snow\" Course")
    }

    @Test("Bundled example data is available to the Swift package")
    func bundledData() throws {
        #expect(try DataLoader.loadStationsCSV().count == 50)
        #expect(try DataLoader.loadReadingsCSV().count == 50)
        #expect(try DataLoader.loadBasinsGeoJSON().count == 6)
    }

    @Test("Missing bundled resources throw naming the resource")
    func missingResourceThrows() throws {
        let emptyBundle = try makeEmptyBundle()

        func expectMissing(_ resourceName: String, _ load: () throws -> Void) {
            do {
                try load()
                Issue.record("Expected a missing-resource error for \(resourceName)")
            } catch let error as DataLoaderError {
                #expect(error.localizedDescription.contains(resourceName))
            } catch {
                Issue.record("Unexpected error type for \(resourceName): \(error)")
            }
        }

        expectMissing("snotel_stations.csv") { _ = try DataLoader.loadStationsCSV(from: emptyBundle) }
        expectMissing("snotel_readings.csv") { _ = try DataLoader.loadReadingsCSV(from: emptyBundle) }
        expectMissing("basins.json") { _ = try DataLoader.loadBasinsGeoJSON(from: emptyBundle) }
    }
}
