import CoreLocation
import CoreML
import Foundation
import Testing

import Relate

/// A zone that reports whatever offset it is given: `TimeZone` wraps a
/// custom `NSTimeZone` and passes its answers through unchecked.
private final class FixedAnswerZone: NSTimeZone, @unchecked Sendable {
    let offset: Int
    init(offset: Int) {
        self.offset = offset
        super.init()
    }
    required init?(coder: NSCoder) { nil }
    override var name: String { "Test/FixedAnswer" }
    override var data: Data { Data() }
    override func secondsFromGMT(for aDate: Date) -> Int { offset }
    override func abbreviation(for aDate: Date) -> String? { nil }
    override func isDaylightSavingTime(for aDate: Date) -> Bool { false }
    override func daylightSavingTimeOffset(for aDate: Date) -> TimeInterval { 0 }
    override func nextDaylightSavingTimeTransition(after aDate: Date) -> Date? { nil }
}

@Suite struct BridgeTests {
    let conn = try! Database.inMemory().connect()

    private func parse(_ iso: String) throws -> Date {
        try Date.ISO8601FormatStyle(includingFractionalSeconds: true).parse(iso)
    }

    // MARK: Dates

    @Test func aDateBindsAsAUTCDateTimeAndRoundTrips() throws {
        let iso = "2026-11-08T13:00:00.123Z"
        let date = try parse(iso)
        let value = try CypherValue(date)
        #expect(value == .dateTime(.init(
            year: 2026, month: 11, day: 8, hour: 13, minute: 0, second: 0,
            nanosecond: 123_000_000, offsetSeconds: 0, zone: nil)))
        // Through the engine and back: the same instant, exactly.
        let back = try #require(conn.query("RETURN $d", ["d": value]).scalar().dateTimeValue)
        #expect(back.foundationDate == date)
        // And equal, in the engine, to the same instant written as ISO text.
        #expect(try conn.query("RETURN datetime($iso) = $d", ["iso": .string(iso), "d": value]).scalar() == true)
    }

    @Test func microsecondsSurviveARoundTrip() throws {
        let date = try parse("2031-02-03T04:05:06.789Z").addingTimeInterval(0.000_456)
        let first = try CypherValue.DateTime(date)
        #expect(first.nanosecond == 789_456_000)
        #expect(try CypherValue.DateTime(try #require(first.foundationDate)) == first)
    }

    @Test func anOffsetKeepsTheInstantAndShiftsTheWallClock() throws {
        let date = try parse("2026-12-31T23:30:00.000Z")
        let shifted = try CypherValue.DateTime(date, offsetSeconds: 3_600)
        #expect(shifted.year == 2027 && shifted.month == 1 && shifted.day == 1)
        #expect(shifted.hour == 0 && shifted.minute == 30 && shifted.offsetSeconds == 3_600)
        #expect(shifted.zone == nil && shifted.foundationDate == date)
        let zone = try #require(TimeZone(identifier: "America/Los_Angeles"))
        let local = try CypherValue.DateTime(date, in: zone)
        #expect(local.hour == 15 && local.offsetSeconds == -28_800 && local.foundationDate == date)
    }

    @Test func civilArithmeticAgreesWithTheEngineAcrossEras() throws {
        // The bridge does its own proleptic-Gregorian arithmetic; the engine
        // is the reference. Leap days, century rules, pre-1582 and pre-epoch
        // instants all go through both.
        let seconds: [Int64] = [
            0, -1, 86_399, 951_782_400, 951_868_800, 4_107_542_400, -2_208_988_800,
            -12_219_292_800, -12_219_379_200, -30_610_224_000, -62_135_596_800, 253_402_300_799,
            1_772_323_199, 1_709_164_800,
        ]
        for s in seconds {
            let engine = try #require(
                conn.query("RETURN datetime({epochSeconds: $s})", ["s": .int(s)]).scalar().dateTimeValue)
            let bridged = try CypherValue.DateTime(Date(timeIntervalSince1970: Double(s)))
            #expect(bridged == engine, "epoch second \(s)")
            #expect(engine.foundationDate == Date(timeIntervalSince1970: Double(s)), "epoch second \(s)")
        }
    }

    @Test func aNonFiniteDateIsRefused() throws {
        for interval in [Double.nan, .infinity, -.infinity] {
            #expect(try invalidParameterMessage {
                _ = try CypherValue(Date(timeIntervalSinceReferenceDate: interval))
            }.contains("not finite"))
        }
    }

    @Test func aDateOutsideTheSupportedYearsIsRefused() throws {
        let zone = try #require(TimeZone(identifier: "America/Los_Angeles"))
        // Finite, but tens of billions of years away: refused before any
        // integer arithmetic, with or without a zone to consult.
        for interval in [4.0e16, -4.0e16, 9.0e18, -9.0e18, .greatestFiniteMagnitude] {
            let date = Date(timeIntervalSince1970: interval)
            #expect(try invalidParameterMessage { _ = try CypherValue.DateTime(date) }.contains("years"))
            _ = try invalidParameterMessage { _ = try CypherValue.DateTime(date, in: zone) }
            _ = try invalidParameterMessage { _ = try CypherValue.Date(date, in: zone) }
            _ = try invalidParameterMessage { _ = try CypherValue.LocalDateTime(date, in: zone) }
        }
        // Foundation's own distant dates are well inside the range (its
        // distant past is 1 January of year 1 on the Julian calendar, two
        // days earlier on this one).
        #expect(try CypherValue.DateTime(Date.distantPast).year == 0)
        #expect(try CypherValue.DateTime(Date.distantFuture).year == 4001)
    }

    @Test func theSupportedYearsAreTheWallClocks() throws {
        let years = CypherValue.Date.supportedYears
        func clock(
            _ year: Int64, _ month: Int64, _ day: Int64, _ hour: Int64, _ minute: Int64, _ second: Int64,
            offset: Int64 = 0
        ) -> CypherValue.DateTime {
            .init(
                year: year, month: month, day: day, hour: hour, minute: minute, second: second,
                nanosecond: 0, offsetSeconds: offset, zone: nil)
        }
        func refused(_ date: Date, offset: Int64 = 0) throws {
            #expect(try invalidParameterMessage {
                _ = try CypherValue.DateTime(date, offsetSeconds: offset)
            }.contains("years"))
        }
        // A Date this far out steps by four seconds, so these are the last
        // instant of the last year and the first instant after it.
        let lastClock = clock(years.upperBound, 12, 31, 23, 59, 56)
        let last = try #require(lastClock.foundationDate)
        let after = last.addingTimeInterval(4)
        #expect(try CypherValue.DateTime(last) == lastClock)
        try refused(after)
        // An hour west of UTC that instant is still the last evening, and it
        // reads back; an hour east, the last instant is already next year.
        let lastEvening = clock(years.upperBound, 12, 31, 23, 0, 0, offset: -3_600)
        #expect(try CypherValue.DateTime(after, offsetSeconds: -3_600) == lastEvening)
        #expect(lastEvening.foundationDate == after)
        try refused(last, offset: 3_600)
        let anHourEast = try #require(TimeZone(secondsFromGMT: 3_600))
        _ = try invalidParameterMessage { _ = try CypherValue.DateTime(last, in: anHourEast) }
        // No offset reaches back from 18 hours into the next year.
        try refused(after.addingTimeInterval(64_800), offset: -64_800)
        // The same at the first instant of the first year.
        let firstClock = clock(years.lowerBound, 1, 1, 0, 0, 0)
        let first = try #require(firstClock.foundationDate)
        let before = first.addingTimeInterval(-4)
        #expect(try CypherValue.DateTime(first) == firstClock)
        try refused(before)
        #expect(try CypherValue.DateTime(before, offsetSeconds: 64_800)
            == clock(years.lowerBound, 1, 1, 17, 59, 56, offset: 64_800))
        try refused(first, offset: -3_600)
        try refused(before.addingTimeInterval(-64_800), offset: 64_800)
        // A wall clock at either end resolves in a zone.
        for year in [years.lowerBound, years.upperBound] {
            let utc = try #require(clock(year, 6, 15, 12, 0, 0).foundationDate)
            let wall = CypherValue.LocalDateTime(
                year: year, month: 6, day: 15, hour: 12, minute: 0, second: 0, nanosecond: 0)
            #expect(wall.foundationDate(in: anHourEast) == utc.addingTimeInterval(-3_600))
        }
    }

    @Test func aZoneReportingAnImpossibleOffsetIsRefused() throws {
        let date = try parse("2026-11-08T13:00:00.000Z")
        let noon = CypherValue.LocalDateTime(
            year: 2026, month: 11, day: 8, hour: 12, minute: 0, second: 0, nanosecond: 0)
        for offset in [Int.min, Int.max, 64_801, -64_801] {
            let zone = FixedAnswerZone(offset: offset) as TimeZone
            #expect(try invalidParameterMessage { _ = try CypherValue.DateTime(date, in: zone) }
                .contains("offset"))
            _ = try invalidParameterMessage { _ = try CypherValue.LocalDateTime(date, in: zone) }
            _ = try invalidParameterMessage { _ = try CypherValue.Date(date, in: zone) }
            #expect(noon.foundationDate(in: zone) == nil)
            #expect(CypherValue.Date(year: 2026, month: 11, day: 8).startOfDay(in: zone) == nil)
        }
        // The same zone with an offset a datetime can carry behaves as any other.
        let east = FixedAnswerZone(offset: 64_800) as TimeZone
        #expect(try CypherValue.DateTime(date, in: east).offsetSeconds == 64_800)
        #expect(noon.foundationDate(in: east) == (try parse("2026-11-07T18:00:00.000Z")))
    }

    @Test func anOffsetBeyondEighteenHoursIsRefused() throws {
        let date = try parse("2026-11-08T13:00:00.000Z")
        #expect(try CypherValue.DateTime(date, offsetSeconds: 64_800).day == 9)
        #expect(try CypherValue.DateTime(date, offsetSeconds: -64_800).day == 7)
        for offset: Int64 in [64_801, -64_801, .max, .min] {
            #expect(try invalidParameterMessage {
                _ = try CypherValue.DateTime(date, offsetSeconds: offset)
            }.contains("offset"))
        }
    }

    @Test func storedComponentsThatNameNoInstantHaveNoDate() throws {
        let zone = try #require(TimeZone(identifier: "America/Los_Angeles"))
        func instant(
            year: Int64 = 2026, month: Int64 = 3, day: Int64 = 15, hour: Int64 = 12,
            minute: Int64 = 0, second: Int64 = 0, nanosecond: Int64 = 0, offset: Int64 = 0
        ) -> (zoned: Date?, local: Date?) {
            (
                CypherValue.DateTime(
                    year: year, month: month, day: day, hour: hour, minute: minute, second: second,
                    nanosecond: nanosecond, offsetSeconds: offset, zone: nil
                ).foundationDate,
                CypherValue.LocalDateTime(
                    year: year, month: month, day: day, hour: hour, minute: minute, second: second,
                    nanosecond: nanosecond
                ).foundationDate(in: zone)
            )
        }
        let noon = try parse("2026-03-15T12:00:00.000Z")
        #expect(instant().zoned == noon && instant().local == noon.addingTimeInterval(7 * 3_600))
        // The structs hold any Int64. Values that would overflow the
        // arithmetic, and ones that are simply not a wall clock, read as nil.
        let none: [String: (zoned: Date?, local: Date?)] = [
            "year .max": instant(year: .max), "year .min": instant(year: .min),
            "year 1,000,000,000": instant(year: 1_000_000_000),
            "month 0": instant(month: 0), "month 13": instant(month: 13), "month .min": instant(month: .min),
            "day 0": instant(day: 0), "day 32": instant(day: 32), "31 April": instant(month: 4, day: 31),
            "day .max": instant(day: .max),
            "29 February 2025": instant(year: 2025, month: 2, day: 29),
            "29 February 1900": instant(year: 1900, month: 2, day: 29),
            "hour 24": instant(hour: 24), "hour -1": instant(hour: -1), "minute 60": instant(minute: 60),
            "second 60": instant(second: 60), "second .max": instant(second: .max),
            "nanosecond -1": instant(nanosecond: -1),
            "nanosecond 2,000,000,000": instant(nanosecond: 2_000_000_000),
            "nanosecond .max": instant(nanosecond: .max), "nanosecond .min": instant(nanosecond: .min),
        ]
        for (name, pair) in none {
            #expect(pair.zoned == nil, "DateTime.foundationDate, \(name)")
            #expect(pair.local == nil, "LocalDateTime.foundationDate(in:), \(name)")
        }
        #expect(instant(year: 2024, month: 2, day: 29).zoned != nil)
        #expect(instant(year: 2000, month: 2, day: 29).zoned != nil)
        // An offset is checked before it is subtracted.
        for offset: Int64 in [.min, .max, 64_801, -64_801] {
            #expect(instant(offset: offset).zoned == nil, "offset \(offset)")
        }
        #expect(instant(offset: 64_800).zoned == noon.addingTimeInterval(-64_800))
        #expect(instant(offset: -64_800).zoned == noon.addingTimeInterval(64_800))
        // A leap second: second 59 carrying a second or more of nanoseconds.
        let leap = instant(hour: 23, minute: 59, second: 59, nanosecond: 1_500_000_000)
        #expect(leap.zoned == (try parse("2026-03-16T00:00:00.500Z")))
        #expect(leap.local == (try parse("2026-03-16T07:00:00.500Z")))
        #expect(instant(second: 59, nanosecond: 1_999_999_999).zoned != nil)
        #expect(CypherValue.Date(year: .max, month: 1, day: 1).startOfDay(in: zone) == nil)
        #expect(CypherValue.Date(year: 2026, month: 2, day: 30).startOfDay(in: zone) == nil)
    }

    @Test func aLocalDateTimeResolvesInAZone() throws {
        let zone = try #require(TimeZone(identifier: "America/Los_Angeles"))
        // An ordinary wall clock.
        let noon = CypherValue.LocalDateTime(
            year: 2026, month: 7, day: 4, hour: 12, minute: 0, second: 0, nanosecond: 0)
        #expect(noon.foundationDate(in: zone) == (try parse("2026-07-04T19:00:00.000Z")))
        #expect(try CypherValue.LocalDateTime(try parse("2026-07-04T19:00:00.000Z"), in: zone) == noon)
        // Clocks go back at 02:00 on 2026-11-01: 01:30 occurs twice, and the
        // earlier instant (daylight time) is what the engine's parameter
        // binding resolves a zoned wall clock to as well.
        let twice = CypherValue.LocalDateTime(
            year: 2026, month: 11, day: 1, hour: 1, minute: 30, second: 0, nanosecond: 0)
        let earlier = try parse("2026-11-01T08:30:00.000Z")
        #expect(twice.foundationDate(in: zone) == earlier)
        let zoned = CypherValue.DateTime(
            year: 2026, month: 11, day: 1, hour: 1, minute: 30, second: 0, nanosecond: 0,
            offsetSeconds: 0, zone: "America/Los_Angeles")
        let engine = try #require(conn.query("RETURN $d", ["d": .dateTime(zoned)]).scalar().dateTimeValue)
        #expect(engine.offsetSeconds == -25_200 && engine.foundationDate == earlier)
        // Clocks go forward at 02:00 on 2026-03-08: 02:30 never occurs.
        let never = CypherValue.LocalDateTime(
            year: 2026, month: 3, day: 8, hour: 2, minute: 30, second: 0, nanosecond: 0)
        #expect(never.foundationDate(in: zone) == nil)
    }

    @Test func aDayBridgesToComponentsAndZones() throws {
        let zone = try #require(TimeZone(identifier: "Pacific/Auckland"))
        let date = try parse("2026-11-08T13:00:00.000Z")  // already the 9th in Auckland
        let day = try CypherValue.Date(date, in: zone)
        #expect(day == .init(year: 2026, month: 11, day: 9))
        // Out: tagged Gregorian, so Foundation reads the same day back
        // whatever calendar the device uses.
        let components = day.dateComponents
        #expect(components.year == 2026 && components.month == 11 && components.day == 9)
        #expect(components.calendar?.identifier == .gregorian)
        #expect(try CypherValue.Date(components) == day)
        // In: uncalendared components are Gregorian; incomplete ones name no day.
        #expect(try CypherValue.Date(DateComponents(year: 2026, month: 11, day: 9)) == day)
        #expect(try invalidParameterMessage {
            _ = try CypherValue.Date(DateComponents(year: 2026, month: 11))
        }.contains("a year, a month and a day"))
        // Components of a calendar that counts years differently are not a
        // Cypher date's fields: the same day is year 2569 to a Buddhist calendar.
        let buddhist = Calendar(identifier: .buddhist)
        let noon = try #require(Calendar(identifier: .gregorian).date(from: DateComponents(
            timeZone: zone, year: 2026, month: 11, day: 9, hour: 12)))
        var theirs = buddhist.dateComponents(in: zone, from: noon)
        #expect(theirs.year == 2569)
        #expect(try invalidParameterMessage { _ = try CypherValue.Date(theirs) }.contains("calendar"))
        theirs.calendar = Calendar(identifier: .iso8601)
        theirs.year = 2026
        theirs.era = 1
        #expect(try CypherValue.Date(theirs) == day)
        #expect(day.startOfDay(in: zone) == (try parse("2026-11-08T11:00:00.000Z")))
        #expect(try conn.query("RETURN date('2026-11-09') = $d", ["d": .date(day)]).scalar() == true)
    }

    @Test func aComponentsEraCountsYearsBeforeTheCommonEra() throws {
        func day(era: Int?, year: Int) throws -> CypherValue.Date {
            try CypherValue.Date(DateComponents(era: era, year: year, month: 3, day: 15))
        }
        // Foundation reports a BCE day as era 0 with a positive year; 1 BCE
        // is Cypher's year 0.
        #expect(try day(era: 1, year: 2026) == .init(year: 2026, month: 3, day: 15))
        #expect(try day(era: 0, year: 1) == .init(year: 0, month: 3, day: 15))
        #expect(try day(era: 0, year: 44) == .init(year: -43, month: 3, day: 15))
        // Within an era years count from 1; no other era exists.
        for (era, year) in [(0, 0), (0, -5), (1, 0), (0, Int.min), (2, 5)] {
            #expect(try invalidParameterMessage { _ = try day(era: era, year: year) }.contains("era"))
        }
        // Without an era the year is as written, of either sign.
        #expect(try day(era: nil, year: 0) == .init(year: 0, month: 3, day: 15))
        #expect(try day(era: nil, year: -43) == .init(year: -43, month: 3, day: 15))
    }

    @Test func componentsThatNameNoDayAreRefused() throws {
        func day(_ year: Int, _ month: Int, _ day: Int, era: Int? = nil) throws -> CypherValue.Date {
            try CypherValue.Date(DateComponents(era: era, year: year, month: month, day: day))
        }
        // The leap rule: every fourth year, but a century only every fourth.
        #expect(try day(2024, 2, 29) == .init(year: 2024, month: 2, day: 29))
        #expect(try day(2000, 2, 29) == .init(year: 2000, month: 2, day: 29))
        #expect(try day(-4, 2, 29) == .init(year: -4, month: 2, day: 29))
        #expect(try day(2026, 12, 31) == .init(year: 2026, month: 12, day: 31))
        let noDay = [
            (2026, 0, 15), (2026, 13, 15), (2026, -1, 15), (2026, .min, 15),
            (2026, 3, 0), (2026, 3, 32), (2026, 3, -1), (2026, 4, 31), (2026, 3, .min),
            (2025, 2, 29), (1900, 2, 29), (2026, 2, 30),
        ]
        for (year, month, dayOfMonth) in noDay {
            #expect(
                try invalidParameterMessage { _ = try day(year, month, dayOfMonth) }
                    .contains("not a day of the Gregorian calendar"),
                "\(year)-\(month)-\(dayOfMonth)")
        }
        // Int.max is how Foundation marks a component as undefined, so it
        // reads as a missing one.
        for (month, dayOfMonth) in [(Int.max, 15), (3, Int.max)] {
            #expect(try invalidParameterMessage { _ = try day(2026, month, dayOfMonth) }
                .contains("a year, a month and a day"))
        }
        // The supported years end at 999,999,999 either side, whichever way
        // the year is written.
        let years = CypherValue.Date.supportedYears
        #expect(try day(Int(years.upperBound), 12, 31).year == years.upperBound)
        #expect(try day(Int(years.lowerBound), 1, 1).year == years.lowerBound)
        #expect(try day(1_000_000_000, 1, 1, era: 0).year == years.lowerBound)
        for (year, era) in [(1_000_000_000, nil), (-1_000_000_000, nil), (Int.max, nil), (Int.min, nil),
            (1_000_000_001, 0), (Int.max, 0), (Int.max, 1)] as [(Int, Int?)]
        {
            _ = try invalidParameterMessage { _ = try day(year, 1, 1, era: era) }
        }
    }

    // MARK: Durations

    @Test func aFixedDurationBridgesBothWays() throws {
        let value = try CypherValue.Duration(Swift.Duration.seconds(90) + .milliseconds(250))
        #expect(value == .init(months: 0, days: 0, seconds: 90, nanoseconds: 250_000_000))
        #expect(value.swiftDuration == .milliseconds(90_250))
        let back = try #require(conn.query("RETURN $d", ["d": .duration(value)]).scalar().durationValue)
        #expect(back.swiftDuration == .milliseconds(90_250))
        // A calendar part has no fixed length.
        let calendar = try #require(conn.query("RETURN duration('P1M2DT3S')").scalar().durationValue)
        #expect(calendar.swiftDuration == nil)
        #expect(calendar.dateComponents == DateComponents(month: 1, day: 2, second: 3, nanosecond: 0))
    }

    @Test func aDurationBeyondInt64SecondsIsRefused() throws {
        #expect(try CypherValue.Duration(.seconds(Int64.max)).seconds == .max)
        #expect(try CypherValue.Duration(.seconds(Int64.min)).seconds == .min)
        for tooLong in [Swift.Duration.seconds(Int64.max) + .seconds(1), .seconds(Int64.min) - .seconds(1),
            .seconds(Int64.max) + .nanoseconds(1), .seconds(Int64.max) * 2]
        {
            _ = try invalidParameterMessage { _ = try CypherValue.Duration(tooLong) }
        }
    }

    // MARK: Vectors

    @Test func aFloatVectorRoundTripsExactly() throws {
        let floats: [Float] = [0.1, -2.5, 3.14159, .leastNormalMagnitude, 1e30]
        let value = try CypherValue(vector: floats)
        let vector = try #require(value.vectorValue)
        #expect(vector.dtype == .float32 && vector.count == 5)
        let back = try #require(conn.query("RETURN $v", ["v": value]).scalar().vectorValue)
        #expect(back.dtype == .float32 && back.float32Values == floats)
        #expect(back.doubleValues == floats.map(Double.init))
        let doubles = try CypherValue(vector: [0.1, 0.2])
        #expect(doubles.vectorValue?.dtype == .float && doubles.vectorValue?.doubleValues == [0.1, 0.2])
        let ints = CypherValue.Vector.ints(dtype: .integer8, values: [1, 2, 3])
        #expect(ints.float32Values == nil && ints.doubleValues == nil && ints.count == 3)
    }

    @Test func aDoubleVectorNarrowsToFloatOrRefusesOutOfRange() throws {
        #expect(try CypherValue.Vector.float64([0.1, -2]).float32Values == [Float(0.1), -2])
        // A finite coordinate beyond Float's range has no Float: no
        // infinity, and no clamping to the largest one either.
        let largest = Double(Float.greatestFiniteMagnitude)
        #expect(try CypherValue.Vector.float64([largest, -largest]).float32Values
            == [.greatestFiniteMagnitude, -.greatestFiniteMagnitude])
        #expect(try CypherValue.Vector.float64([largest.nextUp]).float32Values == nil)
        #expect(try CypherValue.Vector.float64([1, -1e300]).float32Values == nil)
        // Infinities and NaN are values a Float holds. The conveniences
        // refuse to build such a vector, so these are spelled as the case.
        func stored(_ values: [Double]) -> CypherValue.Vector { .floats(dtype: .float, values: values) }
        #expect(stored([.infinity, -.infinity]).float32Values == [.infinity, -.infinity])
        #expect(stored([.nan]).float32Values?.first?.isNaN == true)
    }

    @Test func aVectorWithANonFiniteCoordinateIsRefused() throws {
        for bad in [Float.nan, .infinity, -.infinity] {
            #expect(try invalidParameterMessage { _ = try CypherValue(vector: [1, bad, 3]) }
                .contains("coordinate 1"))
            _ = try invalidParameterMessage { _ = try CypherValue.Vector.float32([bad]) }
            _ = try invalidParameterMessage { _ = try CypherValue(vector: [0.5, Double(bad)]) }
            _ = try invalidParameterMessage { _ = try CypherValue.Vector.float64([Double(bad)]) }
            _ = try invalidParameterMessage {
                _ = try CypherValue.Vector(MLShapedArray<Float>(scalars: [1, bad], shape: [2]))
            }
            for dataType in [MLMultiArrayDataType.float32, .double] {
                let array = try MLMultiArray(shape: [1, 2], dataType: dataType)
                array[0] = 1
                array[1] = NSNumber(value: bad)
                _ = try invalidParameterMessage { _ = try CypherValue.Vector(array) }
            }
        }
        // The engine draws the same line for its own vector() function.
        #expect(throws: RelateError.self) {
            try conn.query("RETURN vector([1.0, 0.0 / 0.0], 2, FLOAT32)")
        }
    }

    @Test func aVectorOverTheMaximumCountIsRefused() throws {
        let most = CypherValue.Vector.maximumCount
        #expect(most == 4_096)
        let full = [Float](repeating: 0.5, count: most)
        let over = full + [0.5]
        #expect(try CypherValue.Vector.float32(full).count == most)
        #expect(try CypherValue(vector: full.map(Double.init)).vectorValue?.count == most)
        #expect(try invalidParameterMessage { _ = try CypherValue(vector: over) }.contains("4096"))
        _ = try invalidParameterMessage { _ = try CypherValue(vector: over.map(Double.init)) }
        #expect(try CypherValue.Vector(MLShapedArray<Float>(scalars: full, shape: [1, most])).count == most)
        _ = try invalidParameterMessage {
            _ = try CypherValue.Vector(MLShapedArray<Float>(scalars: over, shape: [most + 1]))
        }
        // A multi-array is counted across its whole shape, whatever it holds.
        for dataType in [MLMultiArrayDataType.float32, .double, .int32] {
            let array = try MLMultiArray(shape: [1, NSNumber(value: most + 1)], dataType: dataType)
            _ = try invalidParameterMessage { _ = try CypherValue.Vector(array) }
            // A new multi-array's memory is not initialized, so fill it.
            let fits = try MLMultiArray(shape: [2, NSNumber(value: most / 2)], dataType: dataType)
            for index in 0..<fits.count { fits[index] = 1 }
            #expect(try CypherValue.Vector(fits).count == most)
        }
    }

    @Test func coreMLArraysBecomeVectors() throws {
        // A [1, 4] output, the shape an embedding model returns.
        let array = try MLMultiArray(shape: [1, 4], dataType: .float32)
        for (index, value) in [Float(0.25), 0.5, 0.75, 1.0].enumerated() {
            array[index] = NSNumber(value: value)
        }
        let vector = try CypherValue.Vector(array)
        #expect(try vector == .float32([0.25, 0.5, 0.75, 1.0]))
        #expect(try CypherValue.Vector(MLShapedArray<Float>(scalars: [1, 2, 3], shape: [3])) == .float32([1, 2, 3]))

        let doubles = try MLMultiArray(shape: [2], dataType: .double)
        doubles[0] = 0.1
        doubles[1] = 0.2
        #expect(try CypherValue.Vector(doubles) == (try .float64([0.1, 0.2])))

        let ints = try MLMultiArray(shape: [2], dataType: .int32)
        ints[0] = 7
        ints[1] = -8
        #expect(try CypherValue.Vector(ints) == .ints(dtype: .integer32, values: [7, -8]))

        if #available(macOS 26, iOS 26, *) {
            let bytes = try MLMultiArray(shape: [2], dataType: .int8)
            _ = try invalidParameterMessage { _ = try CypherValue.Vector(bytes) }
        }
    }

    // MARK: Points

    @Test func aGeographicPointNamesItsCoordinates() throws {
        let missoula = try CypherValue.Point(latitude: 46.87, longitude: -113.99)
        #expect(missoula.srid == 4326 && missoula.x == -113.99 && missoula.y == 46.87 && missoula.z == nil)
        #expect(missoula.crs == .wgs84 && missoula.latitude == 46.87 && missoula.longitude == -113.99)
        #expect(missoula.height == nil)
        // The engine built the same point from its own latitude/longitude map.
        let engine = try #require(
            conn.query("RETURN point({latitude: 46.87, longitude: -113.99})").scalar().pointValue)
        #expect(engine == missoula)
        #expect(try conn.query("RETURN $p.latitude", ["p": .point(missoula)]).scalar() == 46.87)

        let summit = try CypherValue.Point(latitude: 46.87, longitude: -113.99, height: 978)
        #expect(summit.srid == 4979 && summit.crs == .wgs84_3D && summit.height == 978)
    }

    @Test func aGeographicPointOutsideTheGlobeIsRefused() throws {
        // The poles and the antimeridian are on the globe.
        for (latitude, longitude) in [(90.0, 180.0), (-90.0, -180.0), (0.0, 0.0)] {
            let point = try CypherValue.Point(latitude: latitude, longitude: longitude)
            #expect(point.latitude == latitude && point.longitude == longitude)
        }
        for latitude in [90.000_001, -90.000_001, 180, .nan, .infinity, -.infinity] {
            #expect(try invalidParameterMessage {
                _ = try CypherValue.Point(latitude: latitude, longitude: 0)
            }.contains("latitude"))
        }
        for longitude in [180.000_001, -180.000_001, 540, .nan, .infinity, -.infinity] {
            #expect(try invalidParameterMessage {
                _ = try CypherValue.Point(latitude: 0, longitude: longitude)
            }.contains("longitude"))
        }
        for height in [Double.nan, .infinity, -.infinity] {
            #expect(try invalidParameterMessage {
                _ = try CypherValue.Point(latitude: 0, longitude: 0, height: height)
            }.contains("height"))
        }
        // The engine refuses such a latitude in its own point() function.
        #expect(throws: RelateError.self) { try conn.query("RETURN point({latitude: 91, longitude: 0})") }
    }

    @Test func aCartesianPointHasNoLatitude() throws {
        let flat = CypherValue.Point(x: 1, y: 2)
        #expect(flat.srid == 7203 && flat.crs == .cartesian && flat.latitude == nil && flat.longitude == nil)
        let solid = CypherValue.Point(x: 1, y: 2, z: 3)
        #expect(solid.srid == 9157 && solid.crs == .cartesian3D && solid.height == nil)
        #expect(try conn.query("RETURN point({x: 1, y: 2})").scalar().pointValue == flat)
        #expect(CypherValue.Point(srid: 1234, x: 0, y: 0, z: nil).crs == nil)
        #expect(CypherValue.Point.CRS.allCases.filter(\.isGeographic) == [.wgs84, .wgs84_3D])
    }

    @Test func aPointBridgesToCoreLocation() throws {
        let coordinate = CLLocationCoordinate2D(latitude: 46.87, longitude: -113.99)
        let point = try CypherValue.Point(coordinate)
        #expect(try point == CypherValue.Point(latitude: 46.87, longitude: -113.99))
        #expect(try CypherValue(coordinate) == .point(point))
        let back = try #require(point.coordinate)
        #expect(back.latitude == 46.87 && back.longitude == -113.99)
        #expect(CypherValue.Point(x: 1, y: 2).coordinate == nil)
        // Core Location's own "no coordinate" value, and one off the globe.
        for invalid in [kCLLocationCoordinate2DInvalid, CLLocationCoordinate2D(latitude: 95, longitude: 10)] {
            #expect(!CLLocationCoordinate2DIsValid(invalid))
            _ = try invalidParameterMessage { _ = try CypherValue.Point(invalid) }
            _ = try invalidParameterMessage { _ = try CypherValue(invalid) }
        }
    }
}
