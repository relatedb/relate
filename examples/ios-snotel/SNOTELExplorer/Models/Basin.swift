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

/// One place of the snowpack ranking: a basin that has readings, with their
/// mean SWE.
struct RankedBasin: Identifiable, Equatable {
    let basin: Basin
    let swe: Double

    var id: Basin.ID { basin.id }
}

extension [Basin] {
    /// The basins with readings, highest mean SWE first: the ranking the
    /// filter panel and the insights chart show. A basin without readings
    /// has no place in it.
    var bySWE: [RankedBasin] {
        compactMap { basin in basin.avgSWE.map { RankedBasin(basin: basin, swe: $0) } }
            .sorted { $0.swe > $1.swe }
    }
}
