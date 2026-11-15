#if canImport(SwiftUI) && canImport(AppKit)
import AppKit
import SwiftUI
import Testing

@testable import Relate

/// What a hosted view's body was given, in order.
@MainActor private final class Shown {
    var counters: [Int] = []
    var errors: [String] = []
}

/// A view over one live query that records what each evaluation of its body
/// sees.
private struct CounterView: View {
    @LiveQuery(initial: -1, counterValue)
    private var counter: Int
    let shown: Shown

    var body: some View {
        let _ = shown.counters.append(counter)
        let _ = $counter.error.map { shown.errors.append("\($0)") }
        Text("\(counter)")
    }
}

@MainActor @Suite(.timeLimit(.minutes(1))) struct LiveQueryHostedTests {
    /// Lays the hosted view out until `condition` holds: a view outside a
    /// window is updated by its layout pass, and the query's reads return
    /// between passes.
    private func layOut(_ host: NSView, until condition: () -> Bool) async {
        while !condition() {
            host.layoutSubtreeIfNeeded()
            await Task.yield()
        }
    }

    @Test func aViewIsGivenTheValueNowAndAfterACommit() async throws {
        let db = try counterDatabase()
        let shown = Shown()
        let host = NSHostingView(rootView: CounterView(shown: shown).relateDatabase(db))
        await layOut(host) { shown.counters.last == 0 }
        #expect(shown.counters.first == -1)
        try await db.write(incrementCounter)
        await layOut(host) { shown.counters.last == 1 }
        #expect(shown.errors.isEmpty)
    }

    @Test func aViewWithoutADatabaseIsGivenTheError() async throws {
        let shown = Shown()
        let host = NSHostingView(rootView: CounterView(shown: shown))
        await layOut(host) { !shown.errors.isEmpty }
        #expect(shown.errors.last?.contains("relateDatabase") == true && shown.counters.last == -1)
    }
}
#endif
