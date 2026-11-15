import Foundation

/// What the insights screen shows: the graph's counts and its basins, read
/// in one snapshot (`SNOTELQueries.insights`), so the screen follows one
/// live query and its two halves always agree.
struct Insights: Equatable, Sendable {
    let stats: DatabaseStats
    let basins: [Basin]

    /// What the screen holds until its first read returns.
    static let empty = Insights(stats: .empty, basins: [])
}
