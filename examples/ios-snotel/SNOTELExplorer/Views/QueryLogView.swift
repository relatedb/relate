import SwiftUI
import UIKit

struct QueryLogView: View {
    let viewModel: MapViewModel
    @Environment(\.dismiss) private var dismiss
    @State private var searchText = ""

    private var filteredEntries: [QueryLogEntry] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let entries = viewModel.queryHistory.reversed()
        guard !query.isEmpty else { return Array(entries) }
        return entries.filter { entry in
            entry.searchableText.contains { $0.localizedCaseInsensitiveContains(query) }
        }
    }

    /// What the list says when it has no rows: nothing was recorded, or
    /// nothing matches the search.
    private var emptyState: (title: String, description: String) {
        searchText.isEmpty
            ? ("No queries recorded", "Queries appear here as the app uses Relate.")
            : ("No matching queries", "Try a different operation or parameter.")
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    summary
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }

                Section("Newest first") {
                    if filteredEntries.isEmpty {
                        ContentUnavailableView(
                            emptyState.title,
                            systemImage: "text.magnifyingglass",
                            description: Text(emptyState.description)
                        )
                    } else {
                        ForEach(filteredEntries) { entry in
                            QueryLogRow(entry: entry)
                        }
                    }
                }
            }
            .listStyle(.insetGrouped)
            .searchable(text: $searchText, prompt: "Search Cypher or parameters")
            .navigationTitle("Cypher Query Log")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
                ToolbarItem(placement: .destructiveAction) {
                    Button("Clear", role: .destructive) {
                        viewModel.clearQueryHistory()
                    }
                    .disabled(viewModel.queryHistory.isEmpty)
                }
            }
        }
    }

    private var summary: some View {
        HStack(spacing: 0) {
            LogMetric(
                value: viewModel.queryHistory.count,
                label: "Queries",
                color: .blue
            )
            Divider().frame(height: 38)
            LogMetric(
                value: viewModel.queryHistory.filter(\.succeeded).count,
                label: "Succeeded",
                color: .green
            )
            Divider().frame(height: 38)
            LogMetric(
                value: viewModel.queryHistory.filter { !$0.succeeded }.count,
                label: "Failed",
                color: .red
            )
        }
        .padding(.vertical, 14)
        .background(.thinMaterial, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
    }
}

private struct LogMetric: View {
    let value: Int
    let label: String
    let color: Color

    var body: some View {
        VStack(spacing: 2) {
            Text(value, format: .number)
                .font(.headline.monospacedDigit())
                .foregroundStyle(color)
            Text(label)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
    }
}

private struct QueryLogRow: View {
    let entry: QueryLogEntry
    @State private var isExpanded = false

    var body: some View {
        DisclosureGroup(isExpanded: $isExpanded) {
            VStack(alignment: .leading, spacing: 10) {
                Text(entry.cypher)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .foregroundStyle(.green)
                    .background(Color(red: 0.055, green: 0.075, blue: 0.1), in: RoundedRectangle(cornerRadius: 10))

                if !entry.parameters.isEmpty {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Parameters")
                            .font(.caption.bold())
                        Text(entry.parameters.map { "$\($0)" }.joined(separator: ", "))
                            .font(.system(.caption2, design: .monospaced))
                            .textSelection(.enabled)
                    }
                }

                if let message = entry.failureMessage {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.red)
                }

                HStack {
                    Label("\(entry.rowCount) rows", systemImage: "tablecells")
                    Spacer()
                    Button {
                        UIPasteboard.general.string = entry.cypher
                    } label: {
                        Label("Copy", systemImage: "doc.on.doc")
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            .padding(.top, 8)
        } label: {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: entry.succeeded ? "checkmark.circle.fill" : "xmark.octagon.fill")
                    .foregroundStyle(entry.succeeded ? .green : .red)
                    .padding(.top, 2)

                VStack(alignment: .leading, spacing: 5) {
                    HStack {
                        Text(entry.operation)
                            .font(.caption.bold().monospaced())
                            .foregroundStyle(.blue)
                        if let label = entry.queryTypeLabel {
                            Text(label)
                                .font(.caption2.bold())
                                .padding(.horizontal, 5)
                                .padding(.vertical, 1)
                                .background(.blue.opacity(0.12), in: Capsule())
                        }
                        Spacer()
                        Text(entry.timestamp, format: .dateTime.hour().minute().second())
                            .font(.caption2.monospacedDigit())
                            .foregroundStyle(.secondary)
                    }
                    Text(entry.cypher.trimmingCharacters(in: .whitespacesAndNewlines))
                        .font(.system(.caption, design: .monospaced))
                        .lineLimit(2)
                    Text("\(entry.durationMilliseconds, format: .number.precision(.fractionLength(2))) ms")
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
        }
    }
}
