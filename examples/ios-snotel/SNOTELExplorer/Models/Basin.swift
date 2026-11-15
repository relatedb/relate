import Foundation

/// One row of `SNOTELQueries.basins`. Every column but `avgSWE` (null for a
/// basin without readings) is guaranteed, so a row without one fails to
/// decode, and so does a row whose stored geometry is not a Polygon.
struct Basin: Identifiable, Equatable, Sendable {
    let name: String
    let huc2: String
    /// The basin's outline, for the map overlay: the `geoJSON` column's
    /// text, parsed.
    let outline: BasinOutline
    /// Mean SWE of the basin's station readings; nil when it has none.
    let avgSWE: Double?
    /// How many of the basin's stations report a reading.
    let stationCount: Int

    /// A basin's identity is its HUC2 code.
    var id: String { huc2 }
}

extension Basin: Decodable {
    /// The query's column names.
    private enum CodingKeys: String, CodingKey {
        case name, huc2, geoJSON, avgSWE, stationCount
    }

    /// Decodes one row. The geometry column is text the graph stores, so it
    /// is parsed here: a basin the workbench wrote with a geometry that is
    /// not a GeoJSON Polygon fails the read, naming the column, instead of
    /// reaching the map as something that cannot be drawn.
    init(from decoder: Decoder) throws {
        let row = try decoder.container(keyedBy: CodingKeys.self)
        name = try row.decode(String.self, forKey: .name)
        huc2 = try row.decode(String.self, forKey: .huc2)
        avgSWE = try row.decodeIfPresent(Double.self, forKey: .avgSWE)
        stationCount = try row.decode(Int.self, forKey: .stationCount)
        let geoJSON = try row.decode(String.self, forKey: .geoJSON)
        do {
            outline = try BasinOutline(geoJSON: geoJSON)
        } catch {
            throw DecodingError.dataCorruptedError(
                forKey: .geoJSON, in: row,
                debugDescription: "basin \(name): the stored geometry is not a GeoJSON Polygon (\(error))")
        }
    }
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
