// Proleptic-Gregorian calendar arithmetic — the calendar Cypher's temporal
// values use at every year. Foundation's Gregorian `Calendar` switches to
// the Julian calendar before October 1582, so the Foundation bridges
// (CypherValue+Foundation.swift) convert through these instead.

let secondsPerDay: Int64 = 86_400

extension CypherValue.Date {
    /// The years the Foundation bridges convert: -999,999,999 through
    /// 999,999,999, the range Cypher documents for a temporal value's year.
    /// A bridge refuses a wall clock whose year is outside it, and every
    /// sum in the bridges stays far inside `Int64` for a year in it.
    ///
    /// The engine takes a narrower range as a parameter (-262,143 through
    /// 262,142) and refuses a year outside that when the value is bound.
    public static let supportedYears: ClosedRange<Int64> = -999_999_999...999_999_999
}

/// The UTC offsets a zoned value carries: 18 hours either side, the
/// engine's bound.
let supportedOffsetSeconds: ClosedRange<Int64> = -18 * 3_600...18 * 3_600

/// Floored division and its non-negative remainder (Swift's `/` and `%`
/// truncate toward zero, which is wrong for instants before the epoch).
func floorDivMod(_ value: Int64, _ divisor: Int64) -> (quotient: Int64, remainder: Int64) {
    var quotient = value / divisor
    var remainder = value % divisor
    if remainder < 0 {
        quotient -= 1
        remainder += divisor
    }
    return (quotient, remainder)
}

/// The length of a proleptic-Gregorian month; `nil` when `month` is not
/// 1...12. February follows the 4/100/400 leap rule, which reads the same
/// for years of either sign.
private func daysInMonth(year: Int64, month: Int64) -> Int64? {
    switch month {
    case 1, 3, 5, 7, 8, 10, 12: 31
    case 4, 6, 9, 11: 30
    case 2: year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) ? 29 : 28
    default: nil
    }
}

/// Whether the three name a day of the proleptic-Gregorian calendar in a
/// supported year (year 0 is 1 BCE).
func isCivilDate(year: Int64, month: Int64, day: Int64) -> Bool {
    guard CypherValue.Date.supportedYears.contains(year), let length = daysInMonth(year: year, month: month)
    else { return false }
    return (1...length).contains(day)
}

/// Days from 1970-01-01 to a proleptic-Gregorian civil date (the
/// days-from-civil algorithm). The caller has checked the three with
/// `isCivilDate`.
private func daysFromCivil(year: Int64, month: Int64, day: Int64) -> Int64 {
    let y = month <= 2 ? year - 1 : year
    let era = floorDivMod(y, 400).quotient
    let yearOfEra = y - era * 400
    let dayOfYear = (153 * (month > 2 ? month - 3 : month + 9) + 2) / 5 + day - 1
    let dayOfEra = yearOfEra * 365 + yearOfEra / 4 - yearOfEra / 100 + dayOfYear
    return era * 146_097 + dayOfEra - 719_468
}

/// The proleptic-Gregorian civil date `days` after 1970-01-01 (the inverse
/// of `daysFromCivil`).
private func civilFromDays(_ days: Int64) -> (year: Int64, month: Int64, day: Int64) {
    let shifted = days + 719_468
    let era = floorDivMod(shifted, 146_097).quotient
    let dayOfEra = shifted - era * 146_097
    let yearOfEra = (dayOfEra - dayOfEra / 1_460 + dayOfEra / 36_524 - dayOfEra / 146_096) / 365
    let dayOfYear = dayOfEra - (365 * yearOfEra + yearOfEra / 4 - yearOfEra / 100)
    let monthIndex = (5 * dayOfYear + 2) / 153
    let day = dayOfYear - (153 * monthIndex + 2) / 5 + 1
    let month = monthIndex < 10 ? monthIndex + 3 : monthIndex - 9
    let year = yearOfEra + era * 400 + (month <= 2 ? 1 : 0)
    return (year, month, day)
}

/// Seconds from 1970-01-01T00:00:00 to a civil wall clock, as if that wall
/// clock were UTC; `nil` when the six do not name one (a month 13, a
/// 30 February, an hour 24, a year outside `Date.supportedYears`). The check
/// comes before the arithmetic, so no stored value can overflow it.
func epochSeconds(
    year: Int64, month: Int64, day: Int64, hour: Int64, minute: Int64, second: Int64
) -> Int64? {
    guard isCivilDate(year: year, month: month, day: day),
        (0..<24).contains(hour), (0..<60).contains(minute), (0..<60).contains(second)
    else { return nil }
    return daysFromCivil(year: year, month: month, day: day) * secondsPerDay
        + hour * 3_600 + minute * 60 + second
}

/// The instants a wall clock of the supported years can name at a supported
/// offset, in seconds since 1970: from the first second of the first year
/// to the end of the last, widened by the largest offset either side. An
/// instant inside it is safe to do integer arithmetic on; whether its wall
/// clock is in a supported year is a separate, exact check. Both bounds are
/// whole hours, which a `Double` of this size holds exactly.
let convertibleEpochSeconds: Range<Int64> = {
    let years = CypherValue.Date.supportedYears
    let first = daysFromCivil(year: years.lowerBound, month: 1, day: 1) * secondsPerDay
    let last = daysFromCivil(year: years.upperBound, month: 12, day: 31) * secondsPerDay
    return (first - supportedOffsetSeconds.upperBound)
        ..< (last + secondsPerDay + supportedOffsetSeconds.upperBound)
}()

/// The civil wall clock `seconds` after 1970-01-01T00:00:00 (the inverse of
/// `epochSeconds(year:month:day:hour:minute:second:)`).
func civilFromEpochSeconds(_ seconds: Int64)
    -> (year: Int64, month: Int64, day: Int64, hour: Int64, minute: Int64, second: Int64)
{
    let split = floorDivMod(seconds, secondsPerDay)
    let date = civilFromDays(split.quotient)
    return (
        date.year, date.month, date.day,
        split.remainder / 3_600, split.remainder % 3_600 / 60, split.remainder % 60
    )
}
