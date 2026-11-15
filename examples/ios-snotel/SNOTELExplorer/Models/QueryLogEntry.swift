import Relate
import Foundation

struct QueryLogEntry: Identifiable, Equatable, Sendable {
    enum Outcome: Equatable, Sendable {
        case success
        case failure(String)
    }

    let id: UUID = UUID()
    let timestamp: Date = Date()
    let cypher: String
    /// The names of the parameters the statement bound (`center`,
    /// `radiusMeters`), empty when it bound none. A trace carries names,
    /// not values.
    let parameters: [String]
    let durationMilliseconds: Double
    /// The engine's plan-time classification; `nil` when the statement failed.
    let queryType: QueryType?
    let rowCount: Int
    let outcome: Outcome

    var operation: String {
        cypher
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .split(whereSeparator: \.isWhitespace)
            .first?
            .uppercased() ?? "QUERY"
    }

    /// Short badge text for the engine query type.
    var queryTypeLabel: String? {
        switch queryType {
        case .read: "READ"
        case .write: "WRITE"
        case .readWrite: "READ/WRITE"
        case .schema: "SCHEMA"
        case nil: nil
        }
    }

    var succeeded: Bool { failureMessage == nil }

    /// The bound names as the log shows them: `$center, $radiusMeters`.
    var parametersText: String {
        parameters.map { "$\($0)" }.joined(separator: ", ")
    }

    /// The text a search of the log looks in, which is the text a row
    /// shows: the statement, its operation, its parameters and, for a
    /// failure, the message.
    var searchableText: [String] {
        [cypher, operation, parametersText, failureMessage].compactMap { $0 }
    }

    var failureMessage: String? {
        if case .failure(let message) = outcome { message } else { nil }
    }
}

extension QueryLogEntry {
    /// The log entry of one statement the database ran, from its trace: the
    /// engine's own time for a statement that ran, the wall time of the
    /// call for one that failed before the engine timed it.
    init(_ trace: QueryTrace) {
        self.init(
            cypher: trace.statement,
            parameters: trace.parameterNames,
            durationMilliseconds: trace.error == nil ? trace.elapsedMs : trace.wallMs,
            queryType: trace.queryType,
            rowCount: trace.rowCount,
            outcome: trace.error.map { .failure($0.localizedDescription) } ?? .success)
    }
}
