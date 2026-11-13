import Foundation

/// One row of `SNOTELQueries.basins`: the property names are that
/// query's column names. Every column but `avgSWE` (null for a basin without
/// readings) is guaranteed, so a row without one fails to decode.
struct Basin: Identifiable, Equatable, Decodable, Sendable {
    let name: String
    let huc2: String
    /// The basin's GeoJSON geometry object, for the map overlay.
    let geoJSON: String
    /// Mean SWE of the basin's station readings; nil when it has none.
    let avgSWE: Double?
    /// How many of the basin's stations report a reading.
    let stationCount: Int

    /// A basin's identity is its HUC2 code.
    var id: String { huc2 }
}

extension [Basin] {
    /// The basins with readings, highest mean SWE first: the ranking the
    /// filter panel and the insights chart show.
    var bySWE: [Basin] {
        compactMap { basin in basin.avgSWE.map { (basin, $0) } }
            .sorted { $0.1 > $1.1 }
            .map(\.0)
    }
}
