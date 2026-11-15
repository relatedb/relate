#if canImport(SwiftUI) && canImport(AppKit)
import AppKit
import SwiftUI
import Testing

@testable import Relate

/// What a hosted view's body was given, in order.
@MainActor private final class Shown<Value> {
    var values: [Value] = []
    var errors: [any Error] = []
}

/// A view over one live query of the counter that records what each
/// evaluation of its body sees.
private struct CounterView: View {
    @LiveQuery(initial: -1, counterValue) private var counter: Int
    let shown: Shown<Int>

    var body: some View {
        let _ = shown.values.append(counter)
        let _ = $counter.error.map { shown.errors.append($0) }
        Text("\(counter)")
    }
}

/// A view over a statement query whose statement cannot be built: an
/// unsigned integer above Int64.max has no Cypher form.
private struct UnbuildableView: View {
    @LiveQuery(try "RETURN \(UInt64.max) AS n") private var numbers: [Int]
    let shown: Shown<[Int]>

    var body: some View {
        let _ = shown.values.append(numbers)
        let _ = $numbers.error.map { shown.errors.append($0) }
        Text("\(numbers.count)")
    }
}

@MainActor @Suite(.timeLimit(.minutes(1))) struct LiveQueryHostedTests {
    /// Lays the hosted view out until `condition` holds: a view outside a
    /// window is updated by its layout pass, and the query's reads return
    /// between passes.
    private func layOut(_ host: NSView, until condition: () -> Bool) async {
        await waitUntil {
            host.layoutSubtreeIfNeeded()
            return condition()
        }
    }

    @Test func aViewIsGivenTheValueNowAndAfterACommit() async throws {
        let db = try counterDatabase()
        let shown = Shown<Int>()
        let host = NSHostingView(rootView: CounterView(shown: shown).relateDatabase(db))
        await layOut(host) { shown.values.last == 0 }
        #expect(shown.values.first == -1)
        try await db.write(incrementCounter)
        await layOut(host) { shown.values.last == 1 }
        #expect(shown.errors.isEmpty)
    }

    @Test func aViewWithoutADatabaseIsGivenTheMisuse() async throws {
        let shown = Shown<Int>()
        let host = NSHostingView(rootView: CounterView(shown: shown))
        await layOut(host) { !shown.errors.isEmpty }
        let error = try #require(shown.errors.last as? RelateError)
        #expect(try misuseMessage(of: error).contains("relateDatabase"))
        #expect(shown.values.last == -1)
    }

    @Test func aViewWhoseStatementCannotBeBuiltSeesTheFailureInItsFirstBody() async throws {
        let shown = Shown<[Int]>()
        let host = NSHostingView(rootView: UnbuildableView(shown: shown).relateDatabase(try Database.inMemory()))
        await layOut(host) { !shown.values.isEmpty }
        // Nothing has to be read to know: the failure is there at once.
        let message = try invalidParameterMessage(of: #require(shown.errors.first as? RelateError))
        #expect(message.contains("\(UInt64.max)") && shown.values.first == [])
    }
}
#endif
