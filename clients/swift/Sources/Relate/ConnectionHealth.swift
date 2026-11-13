// MARK: - Poison

extension Connection {
    /// Whether this connection was poisoned by an engine panic and must be
    /// replaced.
    ///
    /// The error channel carries no typed poison signal, so this probes:
    /// it runs `RETURN 1`, and a boundary-stage failure of that statement
    /// means every call on the handle now fails. Call it after an error
    /// whose stage is `.internal`; on `true`, release this connection and
    /// open another from the same ``Database``. Never retry the work that
    /// failed.
    public func isPoisoned() -> Bool {
        do throws(RelateError) {
            _ = try query("RETURN 1")
            return false
        } catch {
            return Connection.probeIndicatesPoison(error)
        }
    }

    /// The verdict on a failed probe: poisoned exactly when the trivial
    /// statement failed at the boundary stage. A statement that trivial
    /// cannot fail any other way on a healthy handle.
    static func probeIndicatesPoison(_ failure: RelateError) -> Bool {
        failure.engineError?.stage == .internal
    }
}
