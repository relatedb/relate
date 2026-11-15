import Foundation
import Relate
@testable import SNOTELCore

/// A bundle rooted at a freshly-created empty directory, so every resource lookup fails.
func makeEmptyBundle() throws -> Bundle {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("SNOTELEmptyBundle-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    guard let bundle = Bundle(url: dir) else {
        struct EmptyBundleUnavailable: Error {}
        throw EmptyBundleUnavailable()
    }
    return bundle
}

/// A ring of positions from `(longitude, latitude)` pairs.
func ring(_ positions: (Double, Double)...) -> [BasinOutline.Position] {
    positions.map { BasinOutline.Position(longitude: $0.0, latitude: $0.1) }
}

/// A 10° square outline, for a test that needs a basin's geometry but not
/// a particular shape.
func squareOutline() throws -> BasinOutline {
    try BasinOutline(rings: [ring((0, 0), (10, 0), (10, 10), (0, 10), (0, 0))])
}

/// A throwaway store holding the three sample stations, their readings and
/// the two sample basins.
func makeLoadedService() async throws -> DatabaseService {
    let svc = try await DatabaseService()
    try await svc.load(
        stations: DataLoader.parseStationsCSV(sampleStationsCSV),
        readings: DataLoader.parseReadingsCSV(sampleReadingsCSV),
        basins: DataLoader.parseBasinsGeoJSON(sampleBasinsJSON)
    )
    return svc
}

/// What a view showing `statement` is given: the app reads stations only
/// this way, through the store's database.
func shownStations(_ statement: Statement, in svc: DatabaseService) async throws -> [Station] {
    try await svc.database.read { tx in try tx.run(statement).decode(Station.self) }
}

func shownStations(_ filter: StationFilter, in svc: DatabaseService) async throws -> [Station] {
    try await shownStations(try SNOTELQueries.stations(filter), in: svc)
}

// MARK: - Sample Test Data

let sampleStationsCSV = """
id,name,state,latitude,longitude,elevation_ft
515,Bogus Basin,ID,43.7667,-116.1,6040
526,Blewett Pass,WA,47.5333,-120.6167,4240
978,Bumping Ridge,WA,46.8833,-121.3,4600
"""

let sampleReadingsCSV = """
station_id,date,swe_inches,snow_depth_inches,temp_avg_f
515,2024-04-01,8.1,22.0,38
526,2024-04-01,14.2,38.0,35
978,2024-04-01,18.7,48.0,33
"""

let sampleBasinsJSON: Data = """
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
