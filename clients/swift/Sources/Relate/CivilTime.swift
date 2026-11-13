// Proleptic-Gregorian calendar arithmetic — the calendar Cypher's temporal
// values use at every year. Foundation's Gregorian `Calendar` switches to
// the Julian calendar before October 1582, so the Foundation bridges
// (CypherValue+Foundation.swift) convert through these instead.

let secondsPerDay: Int64 = 86_400

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

/// Days from 1970-01-01 to a proleptic-Gregorian civil date (the
/// days-from-civil algorithm). `month` is 1...12 and `day` 1...31; years of
/// any sign are valid (year 0 is 1 BCE).
func daysFromCivil(year: Int64, month: Int64, day: Int64) -> Int64 {
    let y = month <= 2 ? year - 1 : year
    let era = floorDivMod(y, 400).quotient
    let yearOfEra = y - era * 400
    let dayOfYear = (153 * (month > 2 ? month - 3 : month + 9) + 2) / 5 + day - 1
    let dayOfEra = yearOfEra * 365 + yearOfEra / 4 - yearOfEra / 100 + dayOfYear
    return era * 146_097 + dayOfEra - 719_468
}

/// The proleptic-Gregorian civil date `days` after 1970-01-01 (the inverse
/// of `daysFromCivil`).
func civilFromDays(_ days: Int64) -> (year: Int64, month: Int64, day: Int64) {
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
/// clock were UTC.
func epochSeconds(
    year: Int64, month: Int64, day: Int64, hour: Int64, minute: Int64, second: Int64
) -> Int64 {
    daysFromCivil(year: year, month: month, day: day) * secondsPerDay
        + hour * 3_600 + minute * 60 + second
}

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
