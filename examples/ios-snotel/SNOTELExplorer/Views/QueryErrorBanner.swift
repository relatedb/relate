import SwiftUI

/// Why a live query on this screen stopped: the error its projected value
/// carries (`$stations.error`).
///
/// A read that throws ends its live query. The value the view holds stays
/// on screen, and the query starts again when its statement changes — for
/// the map, when the filter does.
struct QueryErrorBanner: View {
    let error: any Error

    var body: some View {
        Label(error.localizedDescription, systemImage: "exclamationmark.triangle.fill")
            .font(.caption)
            .padding(10)
            .background(.red.opacity(0.85), in: RoundedRectangle(cornerRadius: 10))
            .foregroundStyle(.white)
            .accessibilityLabel("Unable to update: \(error.localizedDescription)")
    }
}
