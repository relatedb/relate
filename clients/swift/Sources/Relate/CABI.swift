import CRelate
import Foundation

// MARK: - The module's shared C ABI helpers

/// Converts a status into a throw through the call's own `error_json`
/// out-param — the v2 error channel (ADR-0502 §2). No thread-local slot
/// remains, so nothing here depends on the calling thread.
func check(
    _ status: relate_status, _ err: UnsafeMutablePointer<CChar>?, _ what: String
) throws(RelateError) {
    guard status != RELATE_OK else { return }
    throw takeError(err, what)
}

/// The error channel's one decode: the five-key `error_json` document into
/// the typed engine error. A NULL or malformed json is a contract
/// violation through `required`'s single path; a well-formed but unknown
/// document is the typed error it returns, with the raw json riding the
/// refusal text (the channel itself is the contract being violated).
func engineError(from json: UnsafePointer<CChar>?, what: String) -> RelateError {
    struct Wire: Decodable {
        let code: String
        let gqlStatus: String
        let stage: String
        let retryable: Bool
        let message: String
    }
    do {
        let text = try required(json, what)
        let wire = try decodeMetadata(
            Wire.self, text, what: "the error channel is not the documented shape: \(safeWire(text))")
        let stage = try closedWire(EngineError.Stage.self, wire.stage, what: "unknown error stage")
        return .engine(EngineError(
            code: wire.code, gqlStatus: wire.gqlStatus, stage: stage,
            retryable: wire.retryable, message: wire.message))
    } catch {
        return error
    }
}

/// Copies and frees a caller-owned `error_json` out-param through the one
/// error-channel decode — the free is the only thing that differs from the
/// result-owned channel `engineError` also serves.
func takeError(_ err: UnsafeMutablePointer<CChar>?, _ what: String) -> RelateError {
    defer { if let err { relate_free_string(err) } }
    return engineError(from: err, what: what)
}

/// An FFI call that returned OK must have written its out-handle; NULL is
/// a contract violation.
func requireHandle(_ out: OpaquePointer?, _ what: String) throws(RelateError) -> OpaquePointer {
    guard let out else {
        throw RelateError.decoding("\(what) returned OK without a handle")
    }
    return out
}

/// `withCString` stops at the first NUL, which would silently truncate the
/// text the engine sees. The refusal lives inside the crossing helpers, so
/// no call site can forget it.
private func rejectNUL(_ text: String, what: String) throws(RelateError) {
    if text.utf8.contains(0) {
        throw RelateError.invalidParameter("\(what) contains a NUL byte")
    }
}

/// Crosses a required string to the C boundary as its NUL-terminated form:
/// the one crossing for a caller-supplied string, interior-NUL refusal
/// built in.
///
/// `String.withCString` only rethrows an untyped error, so the body's typed
/// failure crosses it as a `Result` — the one way the module keeps
/// `throws(RelateError)` through a standard-library scope function.
func withCString<R>(
    _ text: String, what: String, _ body: (UnsafePointer<CChar>) throws(RelateError) -> R
) throws(RelateError) -> R {
    try rejectNUL(text, what: what)
    let result: Result<R, RelateError> = text.withCString { pointer in
        do throws(RelateError) {
            return .success(try body(pointer))
        } catch {
            return .failure(error)
        }
    }
    return try result.get()
}

/// The length-prefixed twin of `withCString`: crosses a string's UTF-8
/// bytes with their length, so an interior NUL crosses whole.
/// `utf8CString` is NUL-terminated, so its base is never NULL — even an
/// empty string crosses with a real pointer and length zero (the
/// terminator is not part of the length).
func withUTF8<R>(
    _ text: String, _ body: (UnsafePointer<CChar>, Int) throws -> R
) rethrows -> R {
    try text.utf8CString.withUnsafeBufferPointer { chars in
        // The documented never-NULL base: the buffer holds at least the
        // terminator, so the unwrap asserts that invariant.
        try body(chars.baseAddress!, chars.count - 1)
    }
}

/// Runs `body` with an optional string as the C boundary's optional
/// pointer — `nil` stays `nil`, a value crosses through `withCString`'s
/// refusal. The params builder uses it for a child's name and a datetime's
/// zone; `connect` uses it for the session namespace.
func withOptionalCString<R>(
    _ text: String?, what: String, _ body: (UnsafePointer<CChar>?) throws(RelateError) -> R
) throws(RelateError) -> R {
    guard let text else { return try body(nil) }
    return try withCString(text, what: what) { (pointer) throws(RelateError) in try body(pointer) }
}

/// The pointer-shaped twin of `withOptionalCString`: runs `body` with an
/// optional value as the C boundary's optional pointer — `nil` stays
/// `nil`, a value crosses as a pointer to a call-stable copy. The params
/// builder uses it for a point's z coordinate.
func withOptionalPointer<T, R>(
    to value: T?, _ body: (UnsafePointer<T>?) throws -> R
) rethrows -> R {
    guard let value else { return try body(nil) }
    return try withUnsafePointer(to: value) { try body($0) }
}

/// A required NUL-terminated C string from the boundary: NULL is a
/// contract violation for every accessor this is used on (in-range index,
/// non-NULL handle) — never a silent empty string — and so is malformed
/// UTF-8, refused like `utf8` refuses it (never silently replaced with
/// U+FFFD).
func required(_ pointer: UnsafePointer<CChar>?, _ what: String) throws(RelateError) -> String {
    guard let pointer else {
        throw RelateError.decoding("C ABI returned NULL for \(what)")
    }
    return try utf8(pointer, strlen(pointer), what: what)
}

/// A length-prefixed UTF-8 buffer as a `String` — the decode twin of
/// `required`'s NUL-terminated read. Malformed UTF-8 is a contract
/// violation, refused like every other broken-ABI shape — never silently
/// replaced with U+FFFD.
func utf8(_ pointer: UnsafePointer<CChar>, _ length: Int, what: String) throws(RelateError) -> String {
    let bytes = UnsafeRawBufferPointer(start: pointer, count: length)
    guard let text = String(bytes: bytes, encoding: .utf8) else {
        throw RelateError.decoding("\(what) is not valid UTF-8")
    }
    return text
}

/// Decodes the snake_case metadata documents the C ABI returns — the one
/// key-mapping policy for every boundary JSON, the error channel's own
/// document included.
func decodeMetadata<T: Decodable>(
    _: T.Type, _ json: String, what: String
) throws(RelateError) -> T {
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    do {
        return try decoder.decode(T.self, from: Data(json.utf8))
    } catch {
        throw RelateError.decoding("\(what): \(error)")
    }
}
