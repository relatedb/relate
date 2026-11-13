import Foundation

/// One row of `SNOTELQueries`' station projection: the property names
/// are that projection's column names. `sweInches` (an OPTIONAL MATCH) and
/// `distanceMeters` (nearby searches only) are genuinely optional; every
/// other column is guaranteed, so a row without one is a broken query
/// contract and decoding throws, naming the column.
struct Station: Identifiable, Equatable, Decodable, Sendable {
    let id: Int
    let name: String
    let state: String
    let latitude: Double
    let longitude: Double
    let elevation: Int
    var sweInches: Double?
    var distanceMeters: Double?

    var sweLevel: SWELevel { SWELevel.from(swe: sweInches) }
}
