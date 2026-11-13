import Foundation

struct DatabaseStats: Equatable {
    let stations: Int
    let readings: Int
    let basins: Int
    let relationships: Int
    let basinMemberships: Int

    var nodes: Int { stations + readings + basins }

    /// What the insights screen shows until its first read returns.
    static let empty = DatabaseStats(stations: 0, readings: 0, basins: 0, relationships: 0, basinMemberships: 0)
}
