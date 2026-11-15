import Foundation

struct DatabaseStats: Equatable, Sendable {
    let stations: Int
    let readings: Int
    let basins: Int
    let relationships: Int
    let basinMemberships: Int

    var nodes: Int { stations + readings + basins }

    /// The counts of a graph with nothing in it.
    static let empty = DatabaseStats(stations: 0, readings: 0, basins: 0, relationships: 0, basinMemberships: 0)
}
