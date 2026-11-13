import CoreLocation
import CoreML
import Foundation
import Testing

import Relate

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
        #expect(try CypherValue.DateTime(first.foundationDate) == first)
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
            let error = try #require(throws: RelateError.self) {
                try CypherValue(Date(timeIntervalSinceReferenceDate: interval))
            }
            guard case .invalidParameter = error else {
                Issue.record("expected invalidParameter, got \(error)")
                continue
            }
        }
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
        #expect(day.dateComponents == DateComponents(year: 2026, month: 11, day: 9))
        #expect(CypherValue.Date(DateComponents(year: 2026, month: 11, day: 9)) == day)
        #expect(CypherValue.Date(DateComponents(year: 2026, month: 11)) == nil)
        #expect(day.startOfDay(in: zone) == (try parse("2026-11-08T11:00:00.000Z")))
        #expect(try conn.query("RETURN date('2026-11-09') = $d", ["d": .date(day)]).scalar() == true)
    }

    // MARK: Durations

    @Test func aFixedDurationBridgesBothWays() throws {
        let value = CypherValue.Duration(Swift.Duration.seconds(90) + .milliseconds(250))
        #expect(value == .init(months: 0, days: 0, seconds: 90, nanoseconds: 250_000_000))
        #expect(value.swiftDuration == .milliseconds(90_250))
        let back = try #require(conn.query("RETURN $d", ["d": .duration(value)]).scalar().durationValue)
        #expect(back.swiftDuration == .milliseconds(90_250))
        // A calendar part has no fixed length.
        let calendar = try #require(conn.query("RETURN duration('P1M2DT3S')").scalar().durationValue)
        #expect(calendar.swiftDuration == nil)
        #expect(calendar.dateComponents == DateComponents(month: 1, day: 2, second: 3, nanosecond: 0))
    }

    // MARK: Vectors

    @Test func aFloatVectorRoundTripsExactly() throws {
        let floats: [Float] = [0.1, -2.5, 3.14159, .leastNormalMagnitude, 1e30]
        let value = CypherValue(vector: floats)
        let vector = try #require(value.vectorValue)
        #expect(vector.dtype == .float32 && vector.count == 5)
        let back = try #require(conn.query("RETURN $v", ["v": value]).scalar().vectorValue)
        #expect(back.dtype == .float32 && back.float32Values == floats)
        #expect(back.doubleValues == floats.map(Double.init))
        let doubles = CypherValue(vector: [0.1, 0.2])
        #expect(doubles.vectorValue?.dtype == .float && doubles.vectorValue?.doubleValues == [0.1, 0.2])
        let ints = CypherValue.Vector.ints(dtype: .integer8, values: [1, 2, 3])
        #expect(ints.float32Values == nil && ints.doubleValues == nil && ints.count == 3)
    }

    @Test func coreMLArraysBecomeVectors() throws {
        // A [1, 4] output, the shape an embedding model returns.
        let array = try MLMultiArray(shape: [1, 4], dataType: .float32)
        for (index, value) in [Float(0.25), 0.5, 0.75, 1.0].enumerated() {
            array[index] = NSNumber(value: value)
        }
        let vector = try CypherValue.Vector(array)
        #expect(vector == .float32([0.25, 0.5, 0.75, 1.0]))
        #expect(CypherValue.Vector(MLShapedArray<Float>(scalars: [1, 2, 3], shape: [3])) == .float32([1, 2, 3]))

        let doubles = try MLMultiArray(shape: [2], dataType: .double)
        doubles[0] = 0.1
        doubles[1] = 0.2
        #expect(try CypherValue.Vector(doubles) == .float64([0.1, 0.2]))

        let ints = try MLMultiArray(shape: [2], dataType: .int32)
        ints[0] = 7
        ints[1] = -8
        #expect(try CypherValue.Vector(ints) == .ints(dtype: .integer32, values: [7, -8]))

        if #available(macOS 26, iOS 26, *) {
            let bytes = try MLMultiArray(shape: [2], dataType: .int8)
            let error = try #require(throws: RelateError.self) { try CypherValue.Vector(bytes) }
            guard case .invalidParameter = error else {
                Issue.record("expected invalidParameter, got \(error)")
                return
            }
        }
    }

    // MARK: Points

    @Test func aGeographicPointNamesItsCoordinates() throws {
        let missoula = CypherValue.Point(latitude: 46.87, longitude: -113.99)
        #expect(missoula.srid == 4326 && missoula.x == -113.99 && missoula.y == 46.87 && missoula.z == nil)
        #expect(missoula.crs == .wgs84 && missoula.latitude == 46.87 && missoula.longitude == -113.99)
        #expect(missoula.height == nil)
        // The engine built the same point from its own latitude/longitude map.
        let engine = try #require(
            conn.query("RETURN point({latitude: 46.87, longitude: -113.99})").scalar().pointValue)
        #expect(engine == missoula)
        #expect(try conn.query("RETURN $p.latitude", ["p": .point(missoula)]).scalar() == 46.87)

        let summit = CypherValue.Point(latitude: 46.87, longitude: -113.99, height: 978)
        #expect(summit.srid == 4979 && summit.crs == .wgs84_3D && summit.height == 978)
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
        let point = CypherValue.Point(coordinate)
        #expect(point == CypherValue.Point(latitude: 46.87, longitude: -113.99))
        #expect(CypherValue(coordinate) == .point(point))
        let back = try #require(point.coordinate)
        #expect(back.latitude == 46.87 && back.longitude == -113.99)
        #expect(CypherValue.Point(x: 1, y: 2).coordinate == nil)
    }
}
