import Foundation
import CoreLocation
import Relate
import Observation

/// Where first-launch setup stands. The app shows the map only when
/// `ready`, and `ready` is the only state with a store: the open service
/// travels with it.
enum SetupPhase {
    case loading
    case ready(DatabaseService)
    case failed(String)
}

/// The map screen's state: which filter is active, what is selected, and
/// where setup stands.
///
/// It holds no query results. The views read stations, basins and counts
/// with `@LiveQuery`, built from `filter`; changing the filter here changes
/// the statement a view shows, and Relate runs it.
@Observable
@MainActor
final class MapViewModel {
    var queryHistory: [QueryLogEntry] = []
    var selectedStation: Station?
    private(set) var filter: StationFilter = .all
    /// The nearby-search radius. It becomes part of `.nearby` when a search
    /// is made; the slider edits it freely in between.
    var radiusKm: Double = 100
    /// The search field's text; it becomes `.search` only on submit.
    var searchText = ""
    private(set) var phase: SetupPhase = .loading

    private var logConsumer: Task<Void, Never>?

    /// The open store's service; `nil` until setup has finished.
    private var service: DatabaseService? {
        if case .ready(let service) = phase { service } else { nil }
    }

    /// The open database, for the view tree's environment; `nil` until
    /// setup has finished.
    var database: Database? { service?.database }

    /// Opens the durable store and has the `DatabaseService` load the
    /// bundled data on first launch (later launches reopen the checkpointed
    /// graph). Each `await` yields the `MainActor`, so the loading
    /// `ProgressView` keeps animating. There is no first query to run here:
    /// the views' live queries read as soon as they appear.
    func setup(bundle: Bundle? = nil) async {
        guard service == nil else { return }
        phase = .loading
        do {
            let svc = try await DatabaseService(path: DatabaseService.persistentStoreURL().path)
            consumeQueryLog(of: svc)
            try await svc.loadBundledDataIfNeeded(bundle: bundle)
            phase = .ready(svc)
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    /// The basin of an active `.basin` filter, by its identity.
    var selectedBasin: Basin.ID? {
        if case .basin(let huc2) = filter { huc2 } else { nil }
    }

    /// The center of an active `.nearby` filter.
    var nearbyCenter: CLLocationCoordinate2D? {
        if case .nearby(let center, _) = filter { center } else { nil }
    }

    var isFiltered: Bool {
        if case .all = filter { false } else { true }
    }

    func filterByBasin(_ basin: Basin.ID?) {
        apply(basin.map(StationFilter.basin) ?? .all)
    }

    func searchNearby(center: CLLocationCoordinate2D) {
        apply(.nearby(center, radiusKm: radiusKm))
    }

    func showAll() {
        apply(.all)
    }

    func searchStations() {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        apply(query.isEmpty ? .all : .search(query))
    }

    func runDemoQuery(_ cypher: String) async throws -> QueryResult {
        guard let service else {
            throw DemoAppError.databaseNotReady
        }
        return try await service.run(cypher)
    }

    func clearQueryHistory() {
        queryHistory.removeAll()
    }

    /// The one place the filter changes: the search field shows the text of
    /// an active search and is empty otherwise.
    private func apply(_ filter: StationFilter) {
        self.filter = filter
        if case .search(let text) = filter { searchText = text } else { searchText = "" }
    }

    /// The one place query-log entries reach observable state: every
    /// statement the database runs, in execution order, for the service's life.
    private func consumeQueryLog(of svc: DatabaseService) {
        logConsumer?.cancel()
        logConsumer = Task { [weak self, log = svc.queryLog] in
            for await entry in log {
                self?.queryHistory.append(entry)
            }
        }
    }
}

private enum DemoAppError: LocalizedError {
    case databaseNotReady

    var errorDescription: String? {
        "The embedded database is still loading."
    }
}
