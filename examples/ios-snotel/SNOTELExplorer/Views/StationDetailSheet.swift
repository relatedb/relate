import SwiftUI
import CoreLocation

struct StationDetailSheet: View {
    let station: Station
    var viewModel: MapViewModel

    var body: some View {
        NavigationStack {
            List {
                Section("Station Info") {
                    LabeledContent("Name", value: station.name)
                    LabeledContent("ID", value: "\(station.id)")
                    LabeledContent("State", value: station.state)
                    LabeledContent("Elevation", value: "\(station.elevation) ft")
                    LabeledContent("Coordinates",
                                   value: String(format: "%.4f, %.4f", station.latitude, station.longitude))
                }

                Section("Snowpack (Apr 1, 2024)") {
                    if let swe = station.sweInches {
                        HStack {
                            Text("Snow Water Equivalent")
                            Spacer()
                            Text(String(format: "%.1f\"", swe))
                                .fontWeight(.semibold)
                            Circle()
                                .fill(station.sweLevel.color)
                                .frame(width: 12, height: 12)
                        }

                        SWEGaugeView(swe: swe)
                            .frame(height: 24)
                    } else {
                        Text("No data available")
                            .foregroundStyle(.secondary)
                    }
                }

                if let dist = station.distanceMeters {
                    Section("Distance") {
                        LabeledContent("From search center",
                                       value: String(format: "%.1f km", dist / 1000))
                    }
                }

                Section {
                    Button {
                        let center = CLLocationCoordinate2D(
                            latitude: station.latitude,
                            longitude: station.longitude
                        )
                        viewModel.searchNearby(center: center)
                        viewModel.selectedStation = nil
                    } label: {
                        Label("Find Nearby Stations", systemImage: "location.magnifyingglass")
                    }
                }
            }
            .navigationTitle(station.name)
            .navigationBarTitleDisplayMode(.inline)
        }
    }
}

private struct SWEGaugeView: View {
    let swe: Double
    private let maxSWE: Double = 50

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                RoundedRectangle(cornerRadius: 4)
                    .fill(Color(.systemGray5))
                RoundedRectangle(cornerRadius: 4)
                    .fill(SWELevel.from(swe: swe).color)
                    .frame(width: geo.size.width * min(swe / maxSWE, 1.0))
            }
        }
    }
}
