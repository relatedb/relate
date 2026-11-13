import Relate
import SwiftUI

struct ContentView: View {
    @State private var viewModel = MapViewModel()
    @State private var showInsights = false
    @State private var showWorkbench = false
    @State private var showQueryLog = false
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        ZStack(alignment: .bottom) {
            switch viewModel.phase {
            case .loading:
                ProgressView("Loading SNOTEL data...")
            case .failed(let error):
                VStack(spacing: 16) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.largeTitle)
                        .foregroundStyle(.red)
                    Text(error)
                        .multilineTextAlignment(.center)
                        .padding()
                    Button("Try Again") {
                        Task { await viewModel.setup() }
                    }
                    .buttonStyle(.borderedProminent)
                }
            case .ready:
                MapScreen(viewModel: viewModel, filter: viewModel.filter)

                VStack(spacing: 0) {
                    HStack {
                        VStack(spacing: 10) {
                            MapActionButton(
                                title: "Graph insights",
                                systemImage: "chart.xyaxis.line",
                                action: { showInsights = true }
                            )
                            MapActionButton(
                                title: "Cypher workbench",
                                systemImage: "terminal.fill",
                                action: { showWorkbench = true }
                            )
                            MapActionButton(
                                title: "Query history",
                                systemImage: "list.bullet.rectangle.portrait",
                                action: { showQueryLog.toggle() }
                            )
                        }
                        .padding(.leading, 12)
                        Spacer()
                        SWELegendView()
                            .padding(.trailing, 12)
                    }
                    .padding(.top, 60)
                    Spacer()
                }
            }
        }
        .task {
            await viewModel.setup()
        }
        .sheet(item: $viewModel.selectedStation) { station in
            StationDetailSheet(station: station, viewModel: viewModel)
                .presentationDetents([.medium])
        }
        .sheet(isPresented: $showInsights) {
            InsightsView()
        }
        .sheet(isPresented: $showWorkbench) {
            QueryWorkbenchView(viewModel: viewModel)
        }
        .sheet(isPresented: $showQueryLog) {
            QueryLogView(viewModel: viewModel)
                .presentationDetents([.medium, .large])
                .presentationDragIndicator(.visible)
        }
        // Commits are durable as they are made; a checkpoint when the app
        // leaves the foreground keeps the next launch from replaying them.
        .onChange(of: scenePhase) { _, phase in
            if phase == .background, let database = viewModel.database {
                Task { try? await database.checkpointInBackground() }
            }
        }
        // The database every `@LiveQuery` in this view tree reads, the
        // sheets above included. It is nil until setup has opened the store.
        .relateDatabase(viewModel.database)
    }
}

private struct MapActionButton: View {
    let title: String
    let systemImage: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.headline)
                .frame(width: 42, height: 42)
                .background(.ultraThinMaterial, in: Circle())
                .overlay(Circle().stroke(.white.opacity(0.55), lineWidth: 1))
                .shadow(color: .black.opacity(0.12), radius: 5, y: 2)
        }
        .foregroundStyle(.primary)
        .accessibilityLabel(title)
    }
}
