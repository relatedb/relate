import CoreLocation
import Testing
import Foundation
@testable import SNOTELCore

@Suite("MapViewModel")
@MainActor
struct MapViewModelTests {

    @Test("setup() surfaces a missing-resource failure as the failed phase")
    func surfacesLoaderError() async throws {
        let emptyBundle = try makeEmptyBundle()
        let viewModel = MapViewModel()
        await viewModel.setup(bundle: emptyBundle)

        guard case .failed(let message) = viewModel.phase else {
            Issue.record("Expected the failed phase, got \(viewModel.phase)")
            return
        }
        #expect(message.contains("snotel_stations.csv"))
        #expect(viewModel.database == nil)
    }

    @Test("A submitted search becomes the filter, trimmed")
    func searchBecomesTheFilter() {
        let viewModel = MapViewModel()
        viewModel.searchText = "  Blewett  "
        viewModel.searchStations()
        guard case .search("Blewett") = viewModel.filter else {
            Issue.record("Expected a search for the trimmed text, got \(viewModel.filter)")
            return
        }
        #expect(viewModel.searchText == "Blewett")
    }

    @Test("Submitting a blank search clears an active one")
    func blankSearchClears() {
        let viewModel = MapViewModel()
        viewModel.searchText = "Blewett"
        viewModel.searchStations()
        viewModel.searchText = "   "
        viewModel.searchStations()
        #expect(!viewModel.isFiltered && viewModel.searchText.isEmpty)
    }

    @Test("A nearby search takes the radius it was made with")
    func nearbyTakesTheRadius() {
        let viewModel = MapViewModel()
        viewModel.radiusKm = 250
        let center = CLLocationCoordinate2D(latitude: 47.5, longitude: -120.6)
        viewModel.searchNearby(center: center)
        // The slider moves on; the active search keeps its radius until it
        // is made again.
        viewModel.radiusKm = 50
        guard case .nearby(_, radiusKm: 250) = viewModel.filter else {
            Issue.record("Expected the search's own radius, got \(viewModel.filter)")
            return
        }
        #expect(viewModel.nearbyCenter?.latitude == center.latitude && viewModel.selectedBasinName == nil)
    }

    @Test("A basin filter replaces a search")
    func basinReplacesASearch() {
        let viewModel = MapViewModel()
        viewModel.searchText = "WA"
        viewModel.searchStations()
        viewModel.filterByBasin("Pacific Northwest")
        #expect(viewModel.selectedBasinName == "Pacific Northwest" && viewModel.searchText.isEmpty)
    }

    @Test("Choosing no basin shows every station")
    func noBasinShowsAll() {
        let viewModel = MapViewModel()
        viewModel.filterByBasin("Pacific Northwest")
        viewModel.filterByBasin(nil)
        #expect(!viewModel.isFiltered)
    }
}
