import Foundation

// MARK: - Foundation bridges for the temporal values
//
// The bare names `Date` and `Duration` are ambiguous here between the
// nested Cypher types and Foundation's and the standard library's, so both
// are always written qualified.
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

/// The nanoseconds a stored second may carry: below one second, or up to
/// two for a leap second, which the engine writes as second 59 with a
/// fraction of a second or more.
private let storedNanoseconds: Range<Int64> = 0..<2_000_000_000

/// `convertibleEpochSeconds` as the `Double`s a date is compared with.
private let convertibleIntervals =
    Double(convertibleEpochSeconds.lowerBound)..<Double(convertibleEpochSeconds.upperBound)

/// Seconds since 1970 of a stored wall clock, as if it were UTC; `nil` when
/// the stored components do not name one. The one rule both stored-value
/// bridges read by: the six components must name a second of a supported
/// year, and the nanoseconds must be ones a stored second carries.
private func storedWallSeconds(
    year: Int64, month: Int64, day: Int64, hour: Int64, minute: Int64, second: Int64,
    nanosecond: Int64
) -> Int64? {
    guard storedNanoseconds.contains(nanosecond) else { return nil }
    return epochSeconds(
        year: year, month: month, day: day, hour: hour, minute: minute, second: second)
}

/// A `Foundation.Date` as whole seconds since 1970 plus microseconds in
/// `0..<1_000_000`. A non-finite date, or one too far from the supported
/// years for any offset to bring its wall clock into them, has no datetime.
private func epochMicroseconds(of date: Foundation.Date) throws(RelateError)
    -> (seconds: Int64, microseconds: Int64)
{
    let interval = date.timeIntervalSinceReferenceDate
    guard interval.isFinite else {
        throw RelateError.invalidParameter("a Date that is not finite (\(interval)) has no datetime value")
    }
    let whole = interval.rounded(.down)
    // Compared as `Double`s, before any conversion to an integer: a huge
    // interval fails here instead of trapping.
    guard convertibleIntervals.contains(whole + Double(referenceDateEpochSeconds)) else {
        throw RelateError.invalidParameter(
            "a Date \(interval) seconds from the reference date has no datetime value: "
                + "it is outside the years \(CypherValue.Date.supportedYears)")
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
    /// The checked instant `instant` at `offsetSeconds` from UTC. The offset
    /// is checked before it is added. The year is the wall clock's: that is
    /// the year the value stores, and the one `foundationDate` checks.
    private init(
        _ instant: (seconds: Int64, microseconds: Int64), offsetSeconds: Int64
    ) throws(RelateError) {
        guard supportedOffsetSeconds.contains(offsetSeconds) else {
            throw RelateError.invalidParameter(
                "a UTC offset of \(offsetSeconds) seconds is outside the 18 hours either side a datetime carries")
        }
        let wall = civilFromEpochSeconds(instant.seconds + offsetSeconds)
        guard CypherValue.Date.supportedYears.contains(wall.year) else {
            throw RelateError.invalidParameter(
                "the wall clock at a UTC offset of \(offsetSeconds) seconds falls in year \(wall.year), "
                    + "outside the years \(CypherValue.Date.supportedYears)")
        }
        self.init(
            year: wall.year, month: wall.month, day: wall.day,
            hour: wall.hour, minute: wall.minute, second: wall.second,
            nanosecond: instant.microseconds * nanosecondsPerMicrosecond,
            offsetSeconds: offsetSeconds, zone: nil)
    }

    /// The instant `date` as a zoned datetime at `offsetSeconds` from UTC
    /// (UTC itself by default), to the microsecond. No zone id is recorded,
    /// so the value names exactly that instant.
    ///
    /// Throws ``RelateError/invalidParameter(_:)`` for a non-finite `date`,
    /// for an offset beyond 18 hours either side of UTC, and when the wall
    /// clock at that offset falls in a year outside
    /// ``CypherValue/Date/supportedYears``.
    public init(_ date: Foundation.Date, offsetSeconds: Int64 = 0) throws(RelateError) {
        try self.init(try epochMicroseconds(of: date), offsetSeconds: offsetSeconds)
    }

    /// The instant `date` as a zoned datetime carrying `timeZone`'s UTC
    /// offset at that instant. The zone's id is not recorded: the engine
    /// resolves a named zone from the wall clock, which in a daylight-saving
    /// overlap can name the other of the two instants; the offset alone
    /// round-trips the instant exactly.
    ///
    /// Throws as ``init(_:offsetSeconds:)`` does.
    public init(_ date: Foundation.Date, in timeZone: TimeZone) throws(RelateError) {
        // The date is checked first: the zone is asked only about an
        // instant that has a datetime.
        let instant = try epochMicroseconds(of: date)
        try self.init(instant, offsetSeconds: Int64(timeZone.secondsFromGMT(for: date)))
    }

    /// The instant this datetime names: its wall clock less its UTC offset.
    /// `nil` when the stored components do not name one: a month, day or
    /// time of day that does not exist, a year outside
    /// ``CypherValue/Date/supportedYears``, or an offset beyond 18 hours. A
    /// datetime the engine returned always names one.
    public var foundationDate: Foundation.Date? {
        guard supportedOffsetSeconds.contains(offsetSeconds),
            let wall = storedWallSeconds(
                year: year, month: month, day: day, hour: hour, minute: minute, second: second,
                nanosecond: nanosecond)
        else { return nil }
        return Foundation.Date(epochSeconds: wall - offsetSeconds, nanoseconds: nanosecond)
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
    /// never occurs) there is no such instant and the result is `nil`. It
    /// is `nil` too when the stored components are not a wall clock at all,
    /// as for ``CypherValue/DateTime/foundationDate``, and when `timeZone`
    /// reports an offset beyond 18 hours.
    public func foundationDate(in timeZone: TimeZone) -> Foundation.Date? {
        guard
            let wall = storedWallSeconds(
                year: year, month: month, day: day, hour: hour, minute: minute, second: second,
                nanosecond: nanosecond)
        else { return nil }
        // A zone's offset is its own word (a `TimeZone` can wrap a custom
        // `NSTimeZone`), so it is checked before it is used.
        func zoneOffset(at instant: Int64) -> Int64? {
            let seconds = Int64(timeZone.secondsFromGMT(for: Foundation.Date(epochSeconds: instant)))
            return supportedOffsetSeconds.contains(seconds) ? seconds : nil
        }
        // The instant the wall clock names at the offset the zone shows at
        // `probe`. It is real only when the zone shows that same offset at
        // the instant itself.
        func candidate(probing probe: Int64) -> Int64? {
            guard let offset = zoneOffset(at: probe) else { return nil }
            let instant = wall - offset
            return zoneOffset(at: instant) == offset ? instant : nil
        }
        // The zone's offsets a day either side bracket any transition near
        // the wall clock.
        let earliest = [wall - secondsPerDay, wall + secondsPerDay]
            .compactMap(candidate(probing:))
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

    /// The day `components` name, read as a Gregorian year, month and day.
    /// Components without a calendar are taken as Gregorian.
    ///
    /// An `era`, when present, is part of the year: era 1 is the common
    /// era, and era 0 counts years before it, so year 1 of era 0 (1 BCE) is
    /// Cypher's year 0. Within an era years count from 1. Without an era
    /// the year is taken as written, of either sign.
    ///
    /// Throws ``RelateError/invalidParameter(_:)`` when the components do
    /// not name a day: one of the three is missing; they carry a calendar
    /// which counts years or months differently (a Buddhist, Hebrew or
    /// Islamic calendar's fields are not a Cypher date's); the era is not 0
    /// or 1, or its year is below 1; the month is not 1...12 or the day is
    /// not a day of that month; or the year is outside ``supportedYears``.
    public init(_ components: DateComponents) throws(RelateError) {
        if let calendar = components.calendar,
            ![Calendar.Identifier.gregorian, .iso8601].contains(calendar.identifier)
        {
            throw RelateError.invalidParameter(
                "date components of a \(calendar.identifier) calendar are not a Gregorian year, month and day")
        }
        guard let eraYear = components.year, let month = components.month, let day = components.day
        else {
            throw RelateError.invalidParameter("date components need a year, a month and a day")
        }
        let year: Int
        switch components.era {
        case nil: year = eraYear
        case 1 where eraYear >= 1: year = eraYear
        case 0 where eraYear >= 1: year = 1 - eraYear
        case let era?:
            throw RelateError.invalidParameter("era \(era) with year \(eraYear) names no Gregorian year")
        }
        guard isCivilDate(year: Int64(year), month: Int64(month), day: Int64(day)) else {
            throw RelateError.invalidParameter(
                "year \(year), month \(month), day \(day) is not a day of the Gregorian calendar "
                    + "in the years \(Self.supportedYears)")
        }
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
    /// in a daylight-saving gap there, and when the stored year, month and
    /// day do not name a day.
    public func startOfDay(in timeZone: TimeZone) -> Foundation.Date? {
        CypherValue.LocalDateTime(
            year: year, month: month, day: day, hour: 0, minute: 0, second: 0, nanosecond: 0
        ).foundationDate(in: timeZone)
    }
}

extension CypherValue.Duration {
    /// A fixed-length duration, to the nanosecond (finer fractions are
    /// truncated toward zero). Months and days are zero.
    ///
    /// Throws ``RelateError/invalidParameter(_:)`` for a duration beyond
    /// what `Int64` seconds hold, either side of zero.
    public init(_ duration: Swift.Duration) throws(RelateError) {
        // Checked first: `components` traps on a longer duration, and so
        // does describing one, which is why the message does not show it.
        guard (Swift.Duration.seconds(Int64.min)...Swift.Duration.seconds(Int64.max)).contains(duration)
        else {
            throw RelateError.invalidParameter("a duration beyond Int64 seconds has no Cypher duration")
        }
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
    /// Throws ``RelateError/invalidParameter(_:)`` for a non-finite `date`
    /// and for one in a year outside ``CypherValue/Date/supportedYears``.
    public init(_ date: Foundation.Date) throws(RelateError) {
        self = .dateTime(try DateTime(date))
    }
}
