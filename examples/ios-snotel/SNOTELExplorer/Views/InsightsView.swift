import Charts
import Relate
import SwiftUI

/// The graph at a glance. Its counts and its chart are one live query, so
/// both follow a write made while the sheet is open and the screen has one
/// state to show: the counts are placeholders until the first read
/// returns, and a read that failed is shown, not left as zeros.
struct InsightsView: View {
    @LiveQuery(initial: .empty, SNOTELQueries.insights) private var insights: Insights
    @Environment(\.dismiss) private var dismiss

    private let columns = [
        GridItem(.flexible(), spacing: 12),
        GridItem(.flexible(), spacing: 12),
    ]

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 20) {
                    hero

                    if let error = $insights.error {
                        QueryErrorBanner(error: error)
                    }

                    LazyVGrid(columns: columns, spacing: 12) {
                        MetricCard(value: insights.stats.nodes, label: "Graph nodes", icon: "circle.hexagongrid.fill")
                        MetricCard(value: insights.stats.relationships, label: "Relationships", icon: "arrow.triangle.branch")
                        MetricCard(value: insights.stats.stations, label: "Stations", icon: "mappin.and.ellipse")
                        MetricCard(value: insights.stats.basinMemberships, label: "Spatial links", icon: "point.3.connected.trianglepath.dotted")
                    }
                    .redacted(reason: $insights.isLoading ? .placeholder : [])

                    VStack(alignment: .leading, spacing: 12) {
                        Label("Graph model", systemImage: "point.3.filled.connected.trianglepath.dotted")
                            .font(.headline)
                        GraphSchemaView()
                    }
                    .cardStyle()

                    VStack(alignment: .leading, spacing: 12) {
                        Label("Average SWE by basin", systemImage: "chart.bar.xaxis")
                            .font(.headline)

                        Chart(insights.basins.bySWE) { ranked in
                            BarMark(
                                x: .value("SWE", ranked.swe),
                                y: .value("Basin", ranked.basin.name)
                            )
                            .foregroundStyle(SWELevel.from(swe: ranked.swe).color.gradient)
                            .annotation(position: .trailing) {
                                Text(String(format: "%.1f\"", ranked.swe))
                                    .font(.caption2.monospacedDigit())
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .chartXAxisLabel("Snow Water Equivalent (inches)")
                        .frame(height: 260)
                    }
                    .cardStyle()

                    HStack(spacing: 8) {
                        FeaturePill(title: "Embedded", icon: "internaldrive")
                        FeaturePill(title: "Point index", icon: "scope")
                        FeaturePill(title: "Offline", icon: "wifi.slash")
                    }
                }
                .padding()
            }
            .background(Color(.systemGroupedBackground))
            .navigationTitle("Relate Insights")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    private var hero: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Image(systemName: "cylinder.split.1x2.fill")
                    .font(.title)
                Spacer()
                // Shown while the query is following the store.
                if $insights.isLive {
                    Text("LIVE")
                        .font(.caption2.bold())
                        .padding(.horizontal, 9)
                        .padding(.vertical, 5)
                        .background(.white.opacity(0.18), in: Capsule())
                }
            }
            Text("An embedded spatial graph")
                .font(.title2.bold())
            Text("Relate loads CSV and GeoJSON locally, builds a point index, derives basin relationships, and runs every insight in-process.")
                .font(.subheadline)
                .foregroundStyle(.white.opacity(0.85))
        }
        .foregroundStyle(.white)
        .padding(20)
        .background(
            LinearGradient(
                colors: [.indigo, .blue, .cyan],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            ),
            in: RoundedRectangle(cornerRadius: 22, style: .continuous)
        )
    }
}

private struct MetricCard: View {
    let value: Int
    let label: String
    let icon: String

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: icon)
                .font(.title2)
                .foregroundStyle(.blue)
                .frame(width: 38, height: 38)
                .background(.blue.opacity(0.1), in: RoundedRectangle(cornerRadius: 10))
            VStack(alignment: .leading, spacing: 2) {
                Text(value, format: .number)
                    .font(.title2.bold().monospacedDigit())
                Text(label)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }
        .cardStyle()
    }
}

private struct GraphSchemaView: View {
    var body: some View {
        HStack(spacing: 6) {
            SchemaNode(title: "Station", color: .blue)
            SchemaEdge(title: "HAS_READING")
            SchemaNode(title: "Reading", color: .cyan)
        }
        HStack(spacing: 6) {
            SchemaNode(title: "Station", color: .blue)
            SchemaEdge(title: "IN_BASIN")
            SchemaNode(title: "Basin", color: .indigo)
        }
    }
}

private struct SchemaNode: View {
    let title: String
    let color: Color

    var body: some View {
        Text(title)
            .font(.caption.bold())
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .foregroundStyle(color)
            .background(color.opacity(0.12), in: Capsule())
    }
}

private struct SchemaEdge: View {
    let title: String

    var body: some View {
        HStack(spacing: 2) {
            Rectangle().frame(height: 1)
            Image(systemName: "chevron.right")
        }
        .overlay {
            Text(title)
                .font(.system(size: 7, weight: .semibold))
                .padding(.horizontal, 3)
                .background(Color(.secondarySystemGroupedBackground))
                .offset(y: -8)
        }
        .foregroundStyle(.secondary)
        .frame(maxWidth: .infinity)
    }
}

private struct FeaturePill: View {
    let title: String
    let icon: String

    var body: some View {
        Label(title, systemImage: icon)
            .font(.caption.bold())
            .frame(maxWidth: .infinity)
            .padding(.vertical, 10)
            .background(.thinMaterial, in: Capsule())
    }
}

private extension View {
    func cardStyle() -> some View {
        padding(16)
            .background(Color(.secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
    }
}
