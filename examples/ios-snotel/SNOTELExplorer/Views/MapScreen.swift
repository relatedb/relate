import Relate
import SwiftUI

/// The map and its filter panel, over the two live queries they share.
///
/// Neither view asks for its data and nothing here refreshes it: the
/// stations and the basins are `@LiveQuery` properties, so SwiftUI redraws
/// this view when a commit changes what either statement returns — a
/// workbench statement that adds a station puts it on the map.
struct MapScreen: View {
    let viewModel: MapViewModel
    @LiveQuery private var stations: [Station]
    @LiveQuery(SNOTELQueries.basins) private var basins: [Basin]

    /// `filter` is the view model's active filter. Taking it as a parameter
    /// states this view's dependency on it: when the filter changes the
    /// view is given a new statement, and the live query starts again on
    /// it. The stations on screen stay until the new ones arrive.
    init(viewModel: MapViewModel, filter: StationFilter) {
        self.viewModel = viewModel
        _stations = LiveQuery(try SNOTELQueries.stations(filter))
    }

    var body: some View {
        ZStack(alignment: .bottom) {
            MapContainerView(stations: stations, basins: basins, viewModel: viewModel)
                .ignoresSafeArea()

            VStack(spacing: 8) {
                Spacer()
                // A read that failed leaves the last stations and basins on
                // the map; the query's projected value carries why.
                if let error = $stations.error ?? $basins.error {
                    QueryErrorBanner(error: error)
                }
                FilterPanelView(viewModel: viewModel, stationCount: stations.count, basins: basins)
                    .background(.ultraThinMaterial)
                    .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
            }
            .padding(.horizontal, 8)
            .padding(.bottom, 8)
        }
    }
}
