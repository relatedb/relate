//! Zone resolution: classifying a zone string, resolving an IANA zone to
//! its UTC offset at a wall clock or at a UTC instant, and the
//! [`ResolvedZone`] a `timezone` field names (ADR-0178; extracted from
//! `construct.rs` per ADR-0340).

use chrono::{DateTime, NaiveDateTime, Offset, TimeZone};
use chrono_tz::Tz;

use crate::{Error, Result, Value};

use super::{map_temporal, Temporal};

/// How a zone string is spelled. Classified once so every consumer matches
/// on the kind instead of re-probing the string's shape: the formatting side
/// in `mod.rs` and the resolution side here cannot drift apart.
#[derive(Clone, Copy)]
pub(super) enum ZoneSpec<'a> {
    /// An IANA name (`Europe/Stockholm`).
    Iana(&'a str),
    /// `Z` / `z`: UTC.
    Utc,
    /// A numeric fixed-offset spelling (`+01:00`, `-0500`, …).
    Fixed(&'a str),
    /// A `GMT`/`UTC` prefixed fixed offset (`GMT+03:00`, `UTC`), which the
    /// reference engine keeps verbatim as the value's time zone. Split from
    /// [`ZoneSpec::Fixed`] so a consumer reads the classification instead of
    /// re-probing the leading sign.
    Prefixed(&'a str),
    /// Any other string. The formatting path falls back to the stored
    /// numeric offset for it.
    Other(&'a str),
}

/// The numeric tail of a `GMT`/`UTC` prefixed fixed-offset spelling, or
/// `None` when `z` carries no such prefix. The one prefix rule: the zone
/// classifier and [`parse_offset`] both read it, so the accepted prefix set
/// cannot drift. The probe is ASCII-case-insensitive (`Gmt+01:00`, `uTc`),
/// never a hand-enumerated casing table.
pub(super) fn strip_zone_prefix(z: &str) -> Option<&str> {
    let (prefix, rest) = z.split_at_checked(3)?;
    (prefix.eq_ignore_ascii_case("GMT") || prefix.eq_ignore_ascii_case("UTC")).then_some(rest)
}

/// A `GMT`/`UTC` prefixed fixed-offset spelling (`GMT`, `UTC`, `GMT+03:00`,
/// `UTC-05:30`).
fn is_prefixed_fixed_offset(z: &str) -> bool {
    strip_zone_prefix(z)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('+') || rest.starts_with('-'))
}

pub(super) fn zone_kind(z: &str) -> ZoneSpec<'_> {
    if z.contains('/') {
        ZoneSpec::Iana(z)
    } else if z.eq_ignore_ascii_case("Z") {
        ZoneSpec::Utc
    } else if z.starts_with('+') || z.starts_with('-') {
        ZoneSpec::Fixed(z)
    } else if is_prefixed_fixed_offset(z) {
        ZoneSpec::Prefixed(z)
    } else {
        ZoneSpec::Other(z)
    }
}

/// Why an IANA wall clock could not be resolved to a UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneError {
    /// `zone` does not name a zone in the engine's IANA database.
    UnknownZone,
    /// `local` falls in a DST gap: the wall clock never occurs in `zone`.
    NonexistentWallTime,
}

/// The outcome of resolving a wall clock in an IANA zone — chrono's
/// `LocalResult` under a typed name so its two consumers (the resolver
/// [`zone_offset_at`] and the zoned-literal parse) match on one vocabulary
/// and cannot drift when a new outcome appears.
pub(super) enum LocalResolution {
    /// Exactly one instant maps to the wall clock.
    Unique(DateTime<Tz>),
    /// A DST fall-back overlap: the wall clock occurs twice.
    Ambiguous {
        earlier: DateTime<Tz>,
        later: DateTime<Tz>,
    },
    /// A DST gap: the wall clock never occurs in the zone.
    None,
}

impl LocalResolution {
    /// The one ADR-0178 overlap fold: a unique wall clock is itself, a DST
    /// overlap keeps its earliest (pre-transition) occurrence, a gap has
    /// none — each caller maps `None` to its own error.
    pub(super) fn earliest(self) -> Option<DateTime<Tz>> {
        match self {
            LocalResolution::Unique(dt) => Some(dt),
            LocalResolution::Ambiguous { earlier, .. } => Some(earlier),
            LocalResolution::None => None,
        }
    }
}

/// Resolve the wall clock `local` in `tz` — the one call site of chrono's
/// `from_local_datetime`. Each consumer spells its own policy on the
/// outcome: [`zone_offset_at`] keeps the earliest occurrence (ADR-0178),
/// while the zoned-literal parse keeps its supplied-offset
/// disambiguation.
pub(super) fn resolve_local(tz: Tz, local: NaiveDateTime) -> LocalResolution {
    match tz.from_local_datetime(&local) {
        chrono::LocalResult::Single(dt) => LocalResolution::Unique(dt),
        chrono::LocalResult::Ambiguous(earlier, later) => {
            LocalResolution::Ambiguous { earlier, later }
        }
        chrono::LocalResult::None => LocalResolution::None,
    }
}

/// Resolve the IANA wall clock `local` in `zone` to its UTC offset in
/// seconds (ADR-0178).
///
/// A unique wall time yields its offset; a DST overlap yields the earliest
/// (pre-transition) occurrence's offset. An unknown `zone` name and a wall
/// time inside a DST gap are the two error outcomes.
pub fn zone_offset_at(zone: &str, local: NaiveDateTime) -> std::result::Result<i32, ZoneError> {
    let tz: Tz = zone.parse().map_err(|_| ZoneError::UnknownZone)?;
    let dt = resolve_local(tz, local)
        .earliest()
        .ok_or(ZoneError::NonexistentWallTime)?;
    Ok(dt.offset().fix().local_minus_utc())
}

/// How a wall clock stands in an IANA zone, before a caller applies its
/// policy. [`resolve_local`] folds a gap into `None`; construction needs the
/// two offsets on either side to shift the clock forward (Neo4j's
/// `ZonedDateTime` rule), so they are carried here.
pub(super) enum WallClock {
    /// Exactly one offset maps to the wall clock.
    Unique { offset: i32 },
    /// A DST fall-back overlap: the wall clock occurs twice.
    Ambiguous { earlier: i32, later: i32 },
    /// A DST gap: `local` never occurs; the zone's offset steps from
    /// `before` to `after` across the transition.
    Gap { before: i32, after: i32 },
}

/// The offset at a UTC instant, without the wall-clock resolution policy.
fn offset_at_utc(tz: Tz, utc: NaiveDateTime) -> i32 {
    tz.from_utc_datetime(&utc).offset().fix().local_minus_utc()
}

/// Classify `local` in `zone` (the wall-clock twin of
/// [`zone_offset_at_utc`]). An unknown zone name is the only error; a gap
/// carries the offsets on either side so a caller can shift forward.
pub(super) fn wall_clock(zone: &str, local: NaiveDateTime) -> Result<WallClock> {
    let tz: Tz = zone.parse().map_err(|_| unknown_zone_err(zone))?;
    Ok(match resolve_local(tz, local) {
        LocalResolution::Unique(dt) => WallClock::Unique {
            offset: dt.offset().fix().local_minus_utc(),
        },
        LocalResolution::Ambiguous { earlier, later } => WallClock::Ambiguous {
            earlier: earlier.offset().fix().local_minus_utc(),
            later: later.offset().fix().local_minus_utc(),
        },
        LocalResolution::None => {
            // The transition is within the gap around `local`; transitions
            // are months apart, so ±15h on the UTC timeline lands on either
            // side of it.
            let before = local
                .checked_sub_signed(chrono::Duration::hours(15))
                .map(|probe| offset_at_utc(tz, probe))
                .ok_or_else(super::range_err)?;
            let after = local
                .checked_add_signed(chrono::Duration::hours(15))
                .map(|probe| offset_at_utc(tz, probe))
                .ok_or_else(super::range_err)?;
            WallClock::Gap { before, after }
        }
    })
}

/// Resolve a wall clock the way Neo4j's constructors do: a unique time
/// stands; an overlap picks the occurrence at `preferred` when that offset
/// is one of the two (a source `datetime`'s stored offset), else the
/// earliest; a gap shifts the wall clock forward by the gap and resolves at
/// the post-transition offset (`datetime('2020-03-29T02:30[Europe/Stockholm]')`
/// is `03:30+02:00`). Returns the resolved wall clock and its offset.
pub(super) fn resolve_wall_forward(
    zone: &str,
    local: NaiveDateTime,
    preferred: Option<i32>,
) -> Result<(NaiveDateTime, i32)> {
    match wall_clock(zone, local)? {
        WallClock::Unique { offset } => Ok((local, offset)),
        WallClock::Ambiguous { earlier, later } => {
            let offset = match preferred {
                Some(want) if want == earlier => earlier,
                Some(want) if want == later => later,
                _ => earlier,
            };
            Ok((local, offset))
        }
        WallClock::Gap { before, after } => {
            let shift = after - before;
            if shift <= 0 {
                return Err(ambiguous_datetime_err());
            }
            let shifted = local
                .checked_add_signed(chrono::Duration::seconds(i64::from(shift)))
                .ok_or_else(super::range_err)?;
            // The shifted clock must now be a unique post-transition time;
            // anything else means the probe did not bracket one gap.
            match wall_clock(zone, shifted)? {
                WallClock::Unique { offset } if offset == after => Ok((shifted, offset)),
                _ => Err(ambiguous_datetime_err()),
            }
        }
    }
}

/// Resolve the IANA zone `zone` at the UTC instant `utc` to its UTC offset
/// in seconds (ADR-0329, ADR-0340). Unlike a wall clock
/// ([`zone_offset_at`]), an instant is never ambiguous, so an unknown zone
/// is the only failure (`ZoneError::NonexistentWallTime` is unreachable
/// here). The Bolt DateTimeZoneIdUTC decoder and UTC-instant zone
/// conversion share this rule.
pub fn zone_offset_at_utc(zone: &str, utc: NaiveDateTime) -> std::result::Result<i32, ZoneError> {
    let tz: Tz = zone.parse().map_err(|_| ZoneError::UnknownZone)?;
    Ok(tz.from_utc_datetime(&utc).offset().fix().local_minus_utc())
}

/// The IANA zone a short zone name (a `z` pattern field, e.g. `CEST`)
/// denotes. Java resolves these through its built-in `ZoneName` table; the
/// engine carries the subset its acceptance suite exercises, and an
/// unrecognized spelling is a typed error at the call site — never a silent
/// zero offset. Day-saving names map to the region whose rules decide the
/// offset (the `CEST` spelling is Europe/Paris summer time).
pub(super) fn zone_from_short_name(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_uppercase().as_str() {
        "CET" | "CEST" => "Europe/Paris",
        "UTC" => "UTC",
        "EST" | "EDT" => "America/New_York",
        "PST" | "PDT" => "America/Los_Angeles",
        "JST" => "Asia/Tokyo",
        _ => return None,
    })
}

/// The IANA region a `Temporal::DateTime` zone names, or `None` for an
/// offset-only zone (no zone, `Z`, a numeric offset, any other spelling).
/// The typed classification behind the Bolt DateTime vs DateTimeZoneId
/// struct choice (ADR-0325), on the same [`zone_kind`] rule the formatter
/// uses.
pub fn zone_region(zone: Option<&str>) -> Option<&str> {
    match zone.map(zone_kind) {
        Some(ZoneSpec::Iana(name)) => Some(name),
        // Every other spelling is offset-only; the classification answers it.
        _ => None,
    }
}

/// The reading an IANA zone offset is resolved at: a local wall-clock time
/// (constructing a zoned value from local fields) or a UTC instant
/// (converting an already-UTC value into the zone). The one discriminator
/// [`offset_from_map`] takes, so the map probe exists once.
pub(super) enum IanaAt {
    Local(NaiveDateTime),
    Utc(NaiveDateTime),
}

/// The one spelling of the "ambiguous datetime" failure: a wall clock a DST
/// overlap leaves ambiguous, or one a DST gap never produces. The resolver's
/// [`ZoneError::NonexistentWallTime`] outcome and the zoned-literal parse
/// arms all consult it, so the text cannot drift (ADR-0178, ADR-0340).
pub(super) fn ambiguous_datetime_err() -> Error {
    Error::ty("ambiguous datetime")
}

/// The one spelling of the "unknown timezone" failure: a zone name the tz
/// database does not carry. Both readings of the name — the resolver's
/// [`ZoneError::UnknownZone`] outcome and the zoned-literal parse — consult
/// it, so the text cannot drift (ADR-0178, ADR-0340).
pub(super) fn unknown_zone_err(zone: &str) -> Error {
    Error::ty(format!("unknown timezone '{zone}'"))
}

/// One failure vocabulary for both readings: the resolver's typed outcome
/// mapped back to this path's message (ADR-0178, ADR-0340).
fn iana_offset_at(zone: &str, at: IanaAt) -> Result<i32> {
    let resolved = match at {
        IanaAt::Local(local) => zone_offset_at(zone, local),
        IanaAt::Utc(utc) => zone_offset_at_utc(zone, utc),
    };
    resolved.map_err(|e| match e {
        ZoneError::UnknownZone => unknown_zone_err(zone),
        ZoneError::NonexistentWallTime => ambiguous_datetime_err(),
    })
}

/// The offset and zone a `timezone` field (or an inherited source) names:
/// the domain concept the constructors carry between signatures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedZone {
    /// UTC offset in seconds.
    pub(super) offset: i32,
    /// The zone spelling to keep, or `None` for an offset-only source.
    pub(super) zone: Option<String>,
}

impl ResolvedZone {
    /// UTC with zone `Z`: the documented default when no zone is named.
    pub(super) fn utc() -> Self {
        Self {
            offset: 0,
            zone: Some("Z".into()),
        }
    }
}

/// The largest UTC offset the offset grammar accepts (`±18:00`), the bound
/// the reference engine enforces on a zone offset. Public so a surface that
/// builds a zoned value from components (the C ABI's params builder,
/// ADR-0502 §4) applies the same bound instead of re-deriving it.
pub const MAX_OFFSET_SECONDS: i64 = 18 * 3600;

/// The one spelling of an invalid offset (`+45`, a non-numeric field, an
/// out-of-range value).
fn invalid_offset(s: &str) -> Error {
    Error::argument_error(format!("invalid timezone offset '{s}'"))
}

/// The UTC offset in seconds a zone spelling denotes: `Z`, a signed
/// numeric spelling — an hour run with optional minute and second fields,
/// compact (`-0130`) or colon-separated (`+01:30`) — or a `GMT`/`UTC`
/// prefixed one (`GMT+03:00`, bare `UTC`). The one offset grammar, beside
/// the classifier it shares [`strip_zone_prefix`] with. The grammar
/// carries at most one leading sign — the sign is optional, so a pattern
/// field reading an unsigned digit run takes it as positive — and the
/// digit-or-colon tail check is the typed rejection that answers a second
/// sign character, never a sign silently folded away.
///
/// The sum is checked through `unsigned_abs`: a user-controlled field can
/// saturate the signed total to `i64::MIN`, whose `abs()` panics in debug
/// builds, so the magnitude — never the signed value — is what the bound
/// compares.
pub(super) fn parse_offset(s: &str) -> Result<i32> {
    if s == "Z" || s == "z" {
        return Ok(0);
    }
    // A `GMT`/`UTC` prefixed fixed offset (`GMT+03:00`, `UTC`) is the same
    // offset as its numeric tail; the prefix is the zone spelling the value
    // renders, not part of the arithmetic.
    let numeric = strip_zone_prefix(s).unwrap_or(s);
    if numeric.is_empty() {
        return Ok(0);
    }
    // One optional sign, then digits: `strip_prefix` strips exactly one
    // occurrence, so a second sign character survives into the tail — and
    // the digit-or-colon check below is the rejection that catches it.
    let sign: i64 = if numeric.starts_with('-') { -1 } else { 1 };
    let rest = numeric.strip_prefix(['+', '-']).unwrap_or(numeric);
    if !rest.chars().all(|c| c.is_ascii_digit() || c == ':') {
        return Err(invalid_offset(s));
    }
    // Fields are user-controlled digit runs; a field that is not a number is
    // a typed rejection, never a silent zero.
    let field = |value: &str| -> Result<i64> { value.parse().map_err(|_| invalid_offset(s)) };
    let (h, m, sec): (i64, i64, i64) = if rest.contains(':') {
        // The split spells the optional minute/second defaulting once:
        // a shorter arm's missing fields are 0, a longer one is the typed
        // rejection — no per-field collect-then-index pair.
        match rest.split(':').collect::<Vec<_>>().as_slice() {
            [h] => (field(h)?, 0, 0),
            [h, m] => (field(h)?, field(m)?, 0),
            [h, m, sec] => (field(h)?, field(m)?, field(sec)?),
            _ => return Err(invalid_offset(s)),
        }
    } else {
        // The digit check above already proved the run is ASCII digits, so
        // the byte-sliced fields below are whole numerals.
        match rest.len() {
            0..=2 => (field(rest)?, 0, 0),
            4 => (field(&rest[..2])?, field(&rest[2..])?, 0),
            6 => (field(&rest[..2])?, field(&rest[2..4])?, field(&rest[4..])?),
            _ => return Err(invalid_offset(s)),
        }
    };
    let total = sign
        * h.saturating_mul(3600)
            .saturating_add(m.saturating_mul(60))
            .saturating_add(sec);
    if total.unsigned_abs() > MAX_OFFSET_SECONDS as u64 {
        return Err(invalid_offset(s));
    }
    i32::try_from(total).map_err(|_| invalid_offset(s))
}

/// The [`ResolvedZone`] a classified zone spelling names at `at` — the one
/// zone-resolution seam: an IANA region and a short zone name (`CET`, `EST`,
/// …) both resolve through the tz database, an offset spelling through
/// [`parse_offset`], and `Z` is UTC. A consumer with a wall clock reads
/// [`offset_from_map_local`]; this seam itself never shifts one.
fn resolved_zone(spec: ZoneSpec<'_>, spelling: &str, at: IanaAt) -> Result<ResolvedZone> {
    Ok(match spec {
        ZoneSpec::Iana(name) | ZoneSpec::Other(name) => ResolvedZone {
            offset: iana_offset_at(name, at)?,
            zone: Some(String::from(spelling)),
        },
        ZoneSpec::Utc => ResolvedZone::utc(),
        ZoneSpec::Fixed(s) | ZoneSpec::Prefixed(s) => ResolvedZone {
            offset: parse_offset(s)?,
            zone: Some(String::from(spelling)),
        },
    })
}

/// The [`ResolvedZone`] a `timezone` string names, resolved at `at`: the
/// single `zone_kind` match behind [`offset_from_map`].
pub(super) fn offset_for_zone(z: &str, at: IanaAt) -> Result<ResolvedZone> {
    resolved_zone(zone_kind(z), z, at)
}

/// The [`ResolvedZone`] a map's `timezone` field names, resolved at `at`
/// (ADR-0340): the one map entry point, for both the local wall-clock and
/// the UTC-instant readings ([`IanaAt`]). A caller with no instant of its
/// own states the `now()` it resolves the zone at. A missing field is the
/// documented UTC default; a present but non-string value is a typed error,
/// never a silent default.
pub(super) fn offset_from_map(
    m: &std::collections::BTreeMap<String, Value>,
    at: IanaAt,
) -> Result<ResolvedZone> {
    match m.get("timezone") {
        None => Ok(ResolvedZone::utc()),
        Some(Value::String(z)) => offset_for_zone(z, at),
        Some(_) => Err(Error::argument_error("timezone must be STRING")),
    }
}

/// A map's inherited zone source: the [`ResolvedZone`] plus whether its
/// stored offset may disambiguate a DST overlap. Only a `datetime` field is
/// authoritative for that (Neo4j's `datetime({datetime: x})` keeps `x`'s
/// offset, while `datetime({date: x, time: x})` resolves the new wall clock
/// at the zone's earliest occurrence).
struct SourceZone {
    zone: ResolvedZone,
    prefer_offset: bool,
}

/// The [`SourceZone`] a map's inherited source contributes, probing the
/// `time` and `datetime` fields in that order: `Time` contributes its
/// offset only, `DateTime` contributes its offset plus zone, and any other
/// value yields `None` (no inheritable source). [`apply_timezone`] resolves
/// the zone at the local wall clock when the map names its own timezone.
fn source_offset_zone(m: &std::collections::BTreeMap<String, Value>) -> Option<SourceZone> {
    for k in ["time", "datetime"] {
        if let Some(t) = map_temporal(m, k) {
            match t {
                Temporal::Time { offset, .. } => {
                    return Some(SourceZone {
                        zone: ResolvedZone {
                            offset: *offset,
                            zone: None,
                        },
                        prefer_offset: false,
                    })
                }
                Temporal::DateTime { offset, zone, .. } => {
                    return Some(SourceZone {
                        zone: ResolvedZone {
                            offset: *offset,
                            zone: zone.clone(),
                        },
                        prefer_offset: k == "datetime",
                    })
                }
                _ => {}
            }
        }
    }
    None
}

/// `utc + offset seconds` is the zone's wall clock at the UTC instant `utc`,
/// erroring instead of panicking when the result would fall outside chrono's
/// range. The additive twin of [`sub_offset`].
pub(super) fn add_offset(utc: NaiveDateTime, offset: i32) -> Result<NaiveDateTime> {
    utc.checked_add_signed(chrono::Duration::seconds(i64::from(offset)))
        .ok_or_else(super::range_err)
}

/// `local - offset seconds`, erroring instead of panicking when the result
/// would fall outside chrono's range.
pub(super) fn sub_offset(local: NaiveDateTime, offset: i32) -> Result<NaiveDateTime> {
    local
        .checked_sub_signed(chrono::Duration::seconds(i64::from(offset)))
        .ok_or_else(super::range_err)
}

/// `(utc, zone)` after inheriting or converting a map timezone. The
/// inherited source is derived from the same map ([`source_offset_zone`]),
/// so callers never thread it.
pub(super) fn apply_timezone(
    local: NaiveDateTime,
    m: &std::collections::BTreeMap<String, Value>,
) -> Result<(NaiveDateTime, ResolvedZone)> {
    let source = source_offset_zone(m);
    if m.contains_key("timezone") {
        if let Some(src) = source {
            // Resolve the source zone at the local wall clock, then read the
            // instant in the explicitly named zone.
            let (local, src_off) = source_wall(&src, local)?;
            let utc = sub_offset(local, src_off)?;
            return Ok((utc, offset_from_map(m, IanaAt::Utc(utc))?));
        }
        let (local, resolved) = offset_from_map_local(m, local, None)?;
        let utc = sub_offset(local, resolved.offset)?;
        return Ok((utc, resolved));
    }
    if let Some(src) = source {
        let (local, offset) = source_wall(&src, local)?;
        let utc = sub_offset(local, offset)?;
        return Ok((
            utc,
            ResolvedZone {
                offset,
                zone: src.zone.zone,
            },
        ));
    }
    Ok((local, ResolvedZone::utc()))
}

/// Resolve an inherited [`SourceZone`] at the local wall clock: an IANA
/// zone resolves with [`resolve_wall_forward`] (a gap shifts the clock, and
/// a `datetime` source's offset disambiguates an overlap); anything else
/// keeps the stored offset. Returns the possibly shifted wall clock and its
/// offset.
fn source_wall(source: &SourceZone, local: NaiveDateTime) -> Result<(NaiveDateTime, i32)> {
    match source.zone.zone.as_deref().map(zone_kind) {
        Some(ZoneSpec::Iana(name)) => resolve_wall_forward(
            name,
            local,
            source.prefer_offset.then_some(source.zone.offset),
        ),
        _ => Ok((local, source.zone.offset)),
    }
}

/// [`offset_from_map`]'s local-wall-clock reading for the constructors: the
/// wall clock (shifted forward when it falls in a DST gap) and the zone it
/// resolves in. `preferred` disambiguates an overlap.
///
/// Only an IANA region has a wall clock to shift or an overlap to
/// disambiguate; every other spelling answers through the one
/// [`resolved_zone`] seam, so `ZoneSpec::Other` means the same thing here as
/// it does in [`offset_for_zone`] (a tz-database name, not a numeric
/// offset).
pub(super) fn offset_from_map_local(
    m: &std::collections::BTreeMap<String, Value>,
    local: NaiveDateTime,
    preferred: Option<i32>,
) -> Result<(NaiveDateTime, ResolvedZone)> {
    match m.get("timezone") {
        None => Ok((local, ResolvedZone::utc())),
        Some(Value::String(z)) => {
            let spec = zone_kind(z);
            Ok(match spec {
                ZoneSpec::Iana(name) => {
                    let (local, offset) = resolve_wall_forward(name, local, preferred)?;
                    (
                        local,
                        ResolvedZone {
                            offset,
                            zone: Some(z.to_string()),
                        },
                    )
                }
                _ => (local, resolved_zone(spec, z, IanaAt::Local(local))?),
            })
        }
        Some(_) => Err(Error::argument_error("timezone must be STRING")),
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    /// ADR-0340: the UTC-instant resolver shares the typed failure
    /// vocabulary; an instant is never ambiguous, so an unknown zone is
    /// the only error.
    #[test]
    fn zone_offset_at_utc_is_typed_and_unambiguous() {
        let at =
            |s: &str| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").expect("test instant");
        // Either side of the 2021-10-31 Stockholm fall-back: two instants,
        // two offsets.
        assert_eq!(
            zone_offset_at_utc("Europe/Stockholm", at("2021-10-31T00:30")),
            Ok(7200)
        );
        assert_eq!(
            zone_offset_at_utc("Europe/Stockholm", at("2021-10-31T01:30")),
            Ok(3600)
        );
        assert_eq!(
            zone_offset_at_utc("Mars/Base", at("2021-10-31T01:30")),
            Err(ZoneError::UnknownZone)
        );
        // The wall-clock resolver's gap outcome cannot arise at an instant:
        // the local reading 2021-03-28T02:30 does not exist, while the UTC
        // instant 00:30 resolves normally (01:30 CET, before the switch).
        let gap = NaiveDate::from_ymd_opt(2021, 3, 28)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        assert_eq!(
            zone_offset_at("Europe/Stockholm", gap),
            Err(ZoneError::NonexistentWallTime)
        );
        let before_transition = NaiveDate::from_ymd_opt(2021, 3, 28)
            .unwrap()
            .and_hms_opt(0, 30, 0)
            .unwrap();
        assert_eq!(
            zone_offset_at_utc("Europe/Stockholm", before_transition),
            Ok(3600)
        );
    }

    /// F-1-27: a missing `timezone` key is the documented UTC default; a
    /// present but non-string value is a typed error, never a silent default.
    #[test]
    fn offset_from_map_distinguishes_missing_from_malformed_timezone() {
        let at = || {
            IanaAt::Local(
                NaiveDate::from_ymd_opt(2021, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            )
        };
        let missing = std::collections::BTreeMap::new();
        assert_eq!(
            offset_from_map(&missing, at()),
            Ok(ResolvedZone {
                offset: 0,
                zone: Some("Z".into()),
            })
        );
        let malformed = [("timezone".into(), Value::Int(42))]
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            offset_from_map(&malformed, at()),
            Err(Error::argument_error("timezone must be STRING"))
        );
    }

    /// LABS-871: the offset grammar carries at most one leading sign — a
    /// second sign character is the digit-or-colon tail check's typed
    /// rejection, never a sign silently folded away: `--01:00` used to
    /// answer `-01:00`.
    #[test]
    fn parse_offset_carries_at_most_one_leading_sign() {
        assert_eq!(parse_offset("+01:00"), Ok(3600));
        assert_eq!(parse_offset("-01:00"), Ok(-3600));
        assert_eq!(parse_offset("-0130"), Ok(-5400));
        assert_eq!(parse_offset("-05"), Ok(-18000));
        assert_eq!(parse_offset("Z"), Ok(0));
        assert_eq!(parse_offset("UTC"), Ok(0));
        assert_eq!(parse_offset("GMT+03:00"), Ok(10800));
        for s in [
            "--01:00",
            "+-01:00",
            "++01:00",
            "-+01:00",
            "UTC+-01:00",
            "GMT--05:00",
        ] {
            assert_eq!(parse_offset(s), Err(invalid_offset(s)), "{s}");
        }
    }

    /// `iana_offset_at` renders the same `unknown timezone` message for
    /// both readings (the message single-sourced with ADR-0340).
    #[test]
    fn iana_offset_at_renders_one_unknown_zone_message() {
        let local = NaiveDate::from_ymd_opt(2021, 3, 28)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let expected = Error::ty("unknown timezone 'Not/AZone'");
        assert_eq!(
            iana_offset_at("Not/AZone", IanaAt::Local(local)),
            Err(expected.clone())
        );
        assert_eq!(
            iana_offset_at("Not/AZone", IanaAt::Utc(local)),
            Err(expected)
        );
    }
}
