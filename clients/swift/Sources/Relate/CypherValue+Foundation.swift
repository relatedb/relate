import Foundation

// MARK: - Foundation bridges for the temporal values
//
// Inside an extension of `CypherValue` or of its nested types, the bare
// names `Date` and `Duration` are the nested Cypher types, so Foundation's
// and the standard library's are always written qualified here.
//
// `Foundation.Date` is a `Double` of seconds: near the present it resolves
// about a quarter of a microsecond. The bridges therefore carry
// microseconds — the finest unit a `Date` holds exactly — and a value that
// started at millisecond or microsecond precision round-trips unchanged for
// instants within roughly two centuries of 2001.

private let microsecondsPerSecond: Int64 = 1_000_000
private let nanosecondsPerMicrosecond: Int64 = 1_000

/// Seconds between 1970-01-01T00:00:00Z and Foundation's reference date.
private let referenceDateEpochSeconds = Int64(Foundation.Date.timeIntervalBetween1970AndReferenceDate)

/// A `Foundation.Date` as whole seconds since 1970 plus microseconds in
/// `0..<1_000_000`. A non-finite date, or one beyond what `Int64` seconds
/// hold, has no datetime.
private func epochMicroseconds(of date: Foundation.Date) throws(RelateError)
    -> (seconds: Int64, microseconds: Int64)
{
    let interval = date.timeIntervalSinceReferenceDate
    let whole = interval.rounded(.down)
    guard interval.isFinite, whole.magnitude < 9.0e18 else {
        throw RelateError.invalidParameter(
            "a Date \(interval) seconds from the reference date has no datetime value")
    }
    // Rounding the fraction can reach a whole second; the split carries it.
    let fraction = floorDivMod(
        Int64(((interval - whole) * Double(microsecondsPerSecond)).rounded()), microsecondsPerSecond)
    return (Int64(whole) + fraction.quotient + referenceDateEpochSeconds, fraction.remainder)
}

extension Foundation.Date {
    /// The date at `seconds` since 1970 plus `nanoseconds`.
    fileprivate init(epochSeconds seconds: Int64, nanoseconds: Int64 = 0) {
        self.init(
            timeIntervalSinceReferenceDate: Double(seconds - referenceDateEpochSeconds)
                + Double(nanoseconds) / 1_000_000_000)
    }
}

extension CypherValue.DateTime {
    /// The instant `date` as a zoned datetime at `offsetSeconds` from UTC
    /// (UTC itself by default), to the microsecond. No zone id is recorded,
    /// so the value names exactly that instant.
    ///
    /// Throws ``RelateError/invalidParameter(_:)`` for a non-finite `date`.
    public init(_ date: Foundation.Date, offsetSeconds: Int64 = 0) throws(RelateError) {
        let instant = try epochMicroseconds(of: date)
        let wall = civilFromEpochSeconds(instant.seconds + offsetSeconds)
        self.init(
            year: wall.year, month: wall.month, day: wall.day,
            hour: wall.hour, minute: wall.minute, second: wall.second,
            nanosecond: instant.microseconds * nanosecondsPerMicrosecond,
            offsetSeconds: offsetSeconds, zone: nil)
    }

    /// The instant `date` as a zoned datetime carrying `timeZone`'s UTC
    /// offset at that instant. The zone's id is not recorded: the engine
    /// resolves a named zone from the wall clock, which in a daylight-saving
    /// overlap can name the other of the two instants; the offset alone
    /// round-trips the instant exactly.
    public init(_ date: Foundation.Date, in timeZone: TimeZone) throws(RelateError) {
        try self.init(date, offsetSeconds: Int64(timeZone.secondsFromGMT(for: date)))
    }

    /// The instant this datetime names: its wall clock less its UTC offset.
    public var foundationDate: Foundation.Date {
        Foundation.Date(
            epochSeconds: epochSeconds(
                year: year, month: month, day: day, hour: hour, minute: minute, second: second)
                - offsetSeconds,
            nanoseconds: nanosecond)
    }
}

extension CypherValue.LocalDateTime {
    /// The wall clock `date` shows in `timeZone`, to the microsecond.
    public init(_ date: Foundation.Date, in timeZone: TimeZone) throws(RelateError) {
        let zoned = try CypherValue.DateTime(date, in: timeZone)
        self.init(
            year: zoned.year, month: zoned.month, day: zoned.day,
            hour: zoned.hour, minute: zoned.minute, second: zoned.second,
            nanosecond: zoned.nanosecond)
    }

    /// The instant at which `timeZone` shows this wall clock. In a
    /// daylight-saving overlap (the wall clock occurs twice) this is the
    /// earlier instant, as the engine resolves a zoned datetime parameter;
    /// in a gap (the wall clock
    /// never occurs) there is no such instant and the result is `nil`.
    public func foundationDate(in timeZone: TimeZone) -> Foundation.Date? {
        let wall = epochSeconds(
            year: year, month: month, day: day, hour: hour, minute: minute, second: second)
        func offset(at instant: Int64) -> Int64 {
            Int64(timeZone.secondsFromGMT(for: Foundation.Date(epochSeconds: instant)))
        }
        // The zone's offsets a day either side bracket any transition near
        // the wall clock; a candidate instant is real when the zone shows
        // the offset it was computed with.
        let earliest = [wall - secondsPerDay, wall + secondsPerDay]
            .map { wall - offset(at: $0) }
            .filter { offset(at: $0) == wall - $0 }
            .min()
        return earliest.map { Foundation.Date(epochSeconds: $0, nanoseconds: nanosecond) }
    }
}

extension CypherValue.Date {
    /// The calendar day `date` falls on in `timeZone`.
    public init(_ date: Foundation.Date, in timeZone: TimeZone) throws(RelateError) {
        let zoned = try CypherValue.DateTime(date, in: timeZone)
        self.init(year: zoned.year, month: zoned.month, day: zoned.day)
    }

    /// The day `components` name, read as a Gregorian year, month and day:
    /// `nil` unless all three are present, and `nil` for components that
    /// carry a calendar which counts years or months differently (a
    /// Buddhist, Hebrew or Islamic calendar's fields are not a Cypher
    /// date's). Components without a calendar are taken as Gregorian.
    public init?(_ components: DateComponents) {
        if let calendar = components.calendar,
            ![Calendar.Identifier.gregorian, .iso8601].contains(calendar.identifier)
        {
            return nil
        }
        guard let year = components.year, let month = components.month, let day = components.day
        else { return nil }
        self.init(year: Int64(year), month: Int64(month), day: Int64(day))
    }

    /// Year, month and day as `DateComponents` carrying the Gregorian
    /// calendar, so Foundation reads them back as the same day whatever
    /// calendar the user's device is set to. No time zone is set.
    public var dateComponents: DateComponents {
        DateComponents(
            calendar: Calendar(identifier: .gregorian), year: Int(year), month: Int(month), day: Int(day))
    }

    /// The instant this day starts in `timeZone`; `nil` when midnight falls
    /// in a daylight-saving gap there.
    public func startOfDay(in timeZone: TimeZone) -> Foundation.Date? {
        CypherValue.LocalDateTime(
            year: year, month: month, day: day, hour: 0, minute: 0, second: 0, nanosecond: 0
        ).foundationDate(in: timeZone)
    }
}

extension CypherValue.Duration {
    /// A fixed-length duration, to the nanosecond (finer fractions are
    /// truncated toward zero). Months and days are zero.
    public init(_ duration: Swift.Duration) {
        let components = duration.components
        self.init(
            months: 0, days: 0, seconds: components.seconds,
            nanoseconds: components.attoseconds / 1_000_000_000)
    }

    /// This duration as a fixed length of time; `nil` when it has a month
    /// or day part, which have no fixed length.
    public var swiftDuration: Swift.Duration? {
        guard months == 0, days == 0 else { return nil }
        return .seconds(seconds) + .nanoseconds(nanoseconds)
    }

    /// The four components as calendar components, for
    /// `Calendar.date(byAdding:to:)`.
    public var dateComponents: DateComponents {
        DateComponents(
            month: Int(months), day: Int(days), second: Int(seconds), nanosecond: Int(nanoseconds))
    }
}

extension CypherValue {
    /// The instant `date` as a UTC `.dateTime`, to the microsecond.
    ///
    /// Throws ``RelateError/invalidParameter(_:)`` for a non-finite `date`.
    public init(_ date: Foundation.Date) throws(RelateError) {
        self = .dateTime(try DateTime(date))
    }
}
