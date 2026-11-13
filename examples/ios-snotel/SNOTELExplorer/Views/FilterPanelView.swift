import SwiftUI

struct FilterPanelView: View {
    @Bindable var viewModel: MapViewModel
    /// How many stations the map shows, and the basins: the screen's live
    /// queries, passed down.
    let stationCount: Int
    let basins: [Basin]
    @State private var isExpanded = false
    /// Debounces live nearby-radius re-runs while the user drags the slider.
    @State private var radiusReRun: Task<Void, Never>?

    var body: some View {
        VStack(spacing: 12) {
            Button {
                withAnimation(.snappy) { isExpanded.toggle() }
            } label: {
                HStack {
                    Label("Explore & Filter", systemImage: "slider.horizontal.3")
                        .font(.headline)
                    Spacer()
                    Text("\(stationCount) stations")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Image(systemName: "chevron.up")
                        .font(.caption.bold())
                        .rotationEffect(.degrees(isExpanded ? 180 : 0))
                        .foregroundStyle(.secondary)
                }
            }
            .buttonStyle(.plain)
            .accessibilityValue(isExpanded ? "Expanded" : "Collapsed")

            if isExpanded {
                Group {
                    HStack(spacing: 8) {
                        Image(systemName: "magnifyingglass")
                            .foregroundStyle(.secondary)
                        TextField("Station name or state", text: $viewModel.searchText)
                            .textInputAutocapitalization(.words)
                            .submitLabel(.search)
                            .onSubmit {
                                viewModel.searchStations()
                                withAnimation(.snappy) { isExpanded = false }
                            }
                        if !viewModel.searchText.isEmpty {
                            Button {
                                viewModel.showAll()
                            } label: {
                                Image(systemName: "xmark.circle.fill")
                                    .foregroundStyle(.secondary)
                            }
                            .accessibilityLabel("Clear station search")
                        }
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .background(.thinMaterial, in: RoundedRectangle(cornerRadius: 10))

                    HStack {
                        Text("Basin")
                            .font(.subheadline)
                        Spacer()
                        Picker("Basin", selection: Binding(
                            get: { viewModel.selectedBasinName },
                            set: { selection in
                                viewModel.filterByBasin(selection)
                                withAnimation(.snappy) { isExpanded = false }
                            }
                        )) {
                            Text("All Basins").tag(String?.none)
                            ForEach(basins) { basin in
                                Text(basin.name).tag(Optional(basin.name))
                            }
                        }
                        .pickerStyle(.menu)
                    }

                    HStack {
                        Text("Nearby: \(Int(viewModel.radiusKm)) km")
                            .font(.subheadline)
                        Slider(value: $viewModel.radiusKm, in: 25...500, step: 25)
                            .accessibilityLabel("Nearby search radius")
                            .accessibilityValue("\(Int(viewModel.radiusKm)) kilometers")
                            .onChange(of: viewModel.radiusKm) {
                                // A new radius is a new statement, so the live
                                // query starts again on it: only while a nearby
                                // search is active, and debounced so dragging the
                                // slider doesn't start one on every step.
                                guard let center = viewModel.nearbyCenter else { return }
                                radiusReRun?.cancel()
                                radiusReRun = Task {
                                    try? await Task.sleep(for: .milliseconds(300))
                                    guard !Task.isCancelled else { return }
                                    viewModel.searchNearby(center: center)
                                }
                            }
                    }

                    if viewModel.isFiltered {
                        Button("Show All Stations") {
                            viewModel.showAll()
                            withAnimation(.snappy) { isExpanded = false }
                        }
                        .font(.subheadline)
                    }

                    if !basins.bySWE.isEmpty {
                        Divider()
                        Text("Average SWE by Basin")
                            .font(.caption.bold())
                            .frame(maxWidth: .infinity, alignment: .leading)
                        ForEach(basins.bySWE) { basin in
                            HStack {
                                Circle()
                                    .fill(SWELevel.from(swe: basin.avgSWE).color)
                                    .frame(width: 8, height: 8)
                                Text(basin.name)
                                    .font(.caption)
                                Spacer()
                                if let avg = basin.avgSWE {
                                    Text(String(format: "%.1f\"", avg))
                                        .font(.caption.monospacedDigit())
                                }
                                Text("(\(basin.stationCount))")
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                .transition(.move(edge: .bottom).combined(with: .opacity))
            } else {
                Text(filterSummary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding()
        .animation(.snappy, value: isExpanded)
    }

    private var filterSummary: String {
        switch viewModel.filter {
        case .basin(let basin):
            return "Showing \(basin) basin • tap to change"
        case .nearby(_, let radiusKm):
            return "Nearby search within \(Int(radiusKm)) km • tap to change"
        case .search(let text):
            return "Search: \(text) • tap to change"
        case .all:
            return "Tap to search stations, compare basins, or set a nearby radius"
        }
    }
}
