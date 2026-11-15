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
            // A statement that trivial cannot fail at the boundary stage
            // on a healthy handle, so on the probe the signal is the verdict.
            return error.isBoundaryFailure
        }
    }
}

extension RelateError {
    /// Whether the engine refused at the boundary stage (`internal`): the
    /// stage every call on a poisoned handle fails at. A healthy handle can
    /// fail there too (the params builder refusing an impossible date), so
    /// on a caller's statement this is the signal to probe, not the verdict.
    var isBoundaryFailure: Bool { engineError?.stage == .internal }
}
