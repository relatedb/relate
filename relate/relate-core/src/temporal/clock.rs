//! The clock-reading temporal constructors: the `.transaction` /
//! `.statement` / `.realtime` spellings, their timezone argument, the
//! `{timezone: …}` map reading, and the per-statement clock pin every
//! `date()` / `time()` / `now()` reading shares.
//!
//! The engine pins one instant per statement ([`QueryClockGuard`]) and keeps
//! no longer-lived transaction clock, so [`ClockMode::Transaction`] reads
//! that same pinned instant while [`ClockMode::Realtime`] reads the wall
//! clock at each call.

use std::collections::BTreeMap;

use chrono::{Local, NaiveDateTime, Offset, TimeZone, Utc};

use crate::{Error, Result, Value};

use super::zone::{add_offset, offset_for_zone, offset_from_map, IanaAt, ResolvedZone};
use super::Temporal;

/// One of the five temporal kinds whose constructor has clock-access
/// spellings. The typed vocabulary behind [`clock_access`], so no reader
/// keeps a second list of base names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockKind {
    Date,
    Time,
    LocalTime,
    DateTime,
    LocalDateTime,
}

impl ClockKind {
    /// The canonical kind table: every variant with its constructor
    /// spelling. [`ClockKind::name`] and [`ClockKind::from_name`] both read
    /// it, so a variant cannot render without parsing.
    const TABLE: &'static [(&'static str, ClockKind)] = &[
        ("date", ClockKind::Date),
        ("time", ClockKind::Time),
        ("localtime", ClockKind::LocalTime),
        ("datetime", ClockKind::DateTime),
        ("localdatetime", ClockKind::LocalDateTime),
    ];

    /// The constructor spelling of the kind (`localdatetime`, …).
    pub fn name(self) -> &'static str {
        Self::TABLE
            .iter()
            .find(|(_, kind)| *kind == self)
            .map(|(name, _)| *name)
            .expect("every ClockKind is in the table")
    }

    /// The inverse of [`ClockKind::name`].
    pub(super) fn from_name(name: &str) -> Option<Self> {
        super::lookup(Self::TABLE, name)
    }

    /// Whether the kind carries a calendar date. The capability model the
    /// field policy (`accepts` and its guards) and the component readers both
    /// read, so a kind's parts are stated once.
    pub(super) fn has_date(self) -> bool {
        matches!(
            self,
            ClockKind::Date | ClockKind::LocalDateTime | ClockKind::DateTime
        )
    }

    /// Whether the kind carries a time of day.
    pub(super) fn has_time(self) -> bool {
        !matches!(self, ClockKind::Date)
    }

    /// Whether the kind carries a UTC offset / time zone.
    pub(super) fn has_zone(self) -> bool {
        matches!(self, ClockKind::Time | ClockKind::DateTime)
    }

    /// Whether the kind has an epoch-map form. Only `datetime` does: the
    /// reference engine rejects `epochSeconds`/`epochMillis` on every other
    /// constructor, so an epoch map of a zoneless kind is a typed rejection
    /// rather than a silently different reading.
    pub(super) fn has_epoch(self) -> bool {
        matches!(self, ClockKind::DateTime)
    }

    /// Whether the kind carries both halves (the `datetime` source field).
    pub(super) fn has_date_and_time(self) -> bool {
        self.has_date() && self.has_time()
    }

    /// Every clock kind, in table order — the enumeration the tests iterate
    /// over the kind vocabulary; a kind's spelling always comes from
    /// [`ClockKind::name`], never a second copy.
    #[cfg(test)]
    pub(super) fn all() -> impl Iterator<Item = ClockKind> {
        Self::TABLE.iter().map(|(_, kind)| *kind)
    }
}

/// The clock a temporal accessor reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockMode {
    Transaction,
    Statement,
    Realtime,
}

impl ClockMode {
    /// The canonical mode table: every variant with its `.suffix` spelling.
    /// [`ClockMode::suffix`] and the private parser both read it.
    const TABLE: &'static [(&'static str, ClockMode)] = &[
        ("transaction", ClockMode::Transaction),
        ("statement", ClockMode::Statement),
        ("realtime", ClockMode::Realtime),
    ];

    /// The `.suffix` spelling of the mode — parse and render share it.
    pub fn suffix(self) -> &'static str {
        Self::TABLE
            .iter()
            .find(|(_, mode)| *mode == self)
            .map(|(suffix, _)| *suffix)
            .expect("every ClockMode is in the table")
    }
}

/// Every clock-access spelling with its typed meaning — the one registry the
/// parser ([`clock_access`]), the advertised catalog ([`clock_spellings`])
/// and the executor's builtin table all read. A row cannot carry a kind and
/// mode its spelling does not name.
pub const CLOCK_SPELLINGS: &[(&str, ClockKind, ClockMode)] = &[
    ("date.realtime", ClockKind::Date, ClockMode::Realtime),
    ("date.statement", ClockKind::Date, ClockMode::Statement),
    ("date.transaction", ClockKind::Date, ClockMode::Transaction),
    (
        "datetime.realtime",
        ClockKind::DateTime,
        ClockMode::Realtime,
    ),
    (
        "datetime.statement",
        ClockKind::DateTime,
        ClockMode::Statement,
    ),
    (
        "datetime.transaction",
        ClockKind::DateTime,
        ClockMode::Transaction,
    ),
    (
        "localdatetime.realtime",
        ClockKind::LocalDateTime,
        ClockMode::Realtime,
    ),
    (
        "localdatetime.statement",
        ClockKind::LocalDateTime,
        ClockMode::Statement,
    ),
    (
        "localdatetime.transaction",
        ClockKind::LocalDateTime,
        ClockMode::Transaction,
    ),
    (
        "localtime.realtime",
        ClockKind::LocalTime,
        ClockMode::Realtime,
    ),
    (
        "localtime.statement",
        ClockKind::LocalTime,
        ClockMode::Statement,
    ),
    (
        "localtime.transaction",
        ClockKind::LocalTime,
        ClockMode::Transaction,
    ),
    ("time.realtime", ClockKind::Time, ClockMode::Realtime),
    ("time.statement", ClockKind::Time, ClockMode::Statement),
    ("time.transaction", ClockKind::Time, ClockMode::Transaction),
];

/// The kind and mode a `<kind>.<mode>` spelling names, or `None` for every
/// other name. Case insensitive, like every Cypher function name.
pub fn clock_access(name: &str) -> Option<(ClockKind, ClockMode)> {
    let lower = name.to_ascii_lowercase();
    CLOCK_SPELLINGS
        .iter()
        .find(|(spelling, _, _)| *spelling == lower)
        .map(|(_, kind, mode)| (*kind, *mode))
}

/// The `<kind>.<mode>` spelling of a clock accessor, for messages — the
/// inverse of [`clock_access`].
pub fn clock_spelling(kind: ClockKind, mode: ClockMode) -> String {
    format!("{}.{}", kind.name(), mode.suffix())
}

/// The typed spelling registry: every `<kind>.<mode>` spelling with the kind
/// and mode it names — the row source for the executor's builtin table, so
/// the table never re-parses a spelling to recover its own payload.
pub fn clock_spelling_rows() -> impl Iterator<Item = (&'static str, ClockKind, ClockMode)> {
    CLOCK_SPELLINGS.iter().copied()
}

/// Every `<kind>.<mode>` spelling — the advertised set.
pub fn clock_spellings() -> impl Iterator<Item = &'static str> {
    clock_spelling_rows().map(|(spelling, _, _)| spelling)
}

/// One clock observation: the instant as a UTC wall clock and the host's
/// local offset at that instant. The local wall clock is derived
/// ([`clock_temporal`]), never stored, so the two readings cannot name
/// different instants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClockReading {
    utc: NaiveDateTime,
    local_offset: i32,
}

impl ClockReading {
    /// The wall-clock observation: sample the instant once, then read the
    /// host's offset at that same instant.
    fn wall_clock() -> Self {
        let utc = Utc::now();
        let naive = utc.naive_utc();
        Self {
            utc: naive,
            local_offset: Local
                .offset_from_utc_datetime(&naive)
                .fix()
                .local_minus_utc(),
        }
    }

    /// Test seam: the observation of an explicit instant at an explicit host
    /// offset, so a test can pin a non-UTC host instead of hiding the offset
    /// behind a zero.
    #[cfg(test)]
    fn pinned(utc: NaiveDateTime, local_offset: i32) -> Self {
        Self { utc, local_offset }
    }
}

/// The zone a clock kind reads in when the caller names none: the host's
/// local reading for the zoneless kinds, UTC (`Z`) for the zoned `datetime`
/// — the engine's documented default zone.
fn default_zone(kind: ClockKind, reading: ClockReading) -> ResolvedZone {
    match kind {
        ClockKind::DateTime => ResolvedZone::utc(),
        ClockKind::Date | ClockKind::Time | ClockKind::LocalTime | ClockKind::LocalDateTime => {
            ResolvedZone {
                offset: reading.local_offset,
                zone: None,
            }
        }
    }
}

/// One zoned `DateTime` from a UTC instant and the zone it is read in — the
/// one zoned-construction seam.
pub(super) fn zoned(utc: NaiveDateTime, resolved: ResolvedZone) -> Temporal {
    Temporal::DateTime {
        utc,
        offset: resolved.offset,
        zone: resolved.zone,
    }
}

/// The temporal value `kind` reads at the UTC instant `utc` in `resolved` —
/// the one construction seam every clock reading goes through (the accessor
/// argument, the clock-reading map, the epoch map, and the `now_*` readers).
pub(super) fn clock_temporal(
    kind: ClockKind,
    utc: NaiveDateTime,
    resolved: ResolvedZone,
) -> Result<Temporal> {
    let wall = add_offset(utc, resolved.offset)?;
    Ok(match kind {
        ClockKind::Date => Temporal::Date(wall.date()),
        ClockKind::Time => Temporal::Time {
            time: wall.time(),
            offset: resolved.offset,
        },
        ClockKind::LocalTime => Temporal::LocalTime(wall.time()),
        ClockKind::DateTime => zoned(utc, resolved),
        ClockKind::LocalDateTime => Temporal::LocalDateTime(wall),
    })
}

/// The clock-reading map form: the current instant (transaction clock) read
/// in the map's `timezone`. A present but non-string timezone is
/// [`offset_from_map`]'s typed rejection, never a silent UTC reading.
pub(super) fn clock_from_map(kind: ClockKind, m: &BTreeMap<String, Value>) -> Result<Temporal> {
    let reading = clock_reading(ClockMode::Transaction);
    let resolved = offset_from_map(m, IanaAt::Utc(reading.utc))?;
    clock_temporal(kind, reading.utc, resolved)
}

/// The clock-access form (`date.transaction('Europe/Paris')`, …): its one
/// argument is a timezone, never a value, so it resolves through the zone
/// table at the accessor's own instant. Arity is the dispatcher's contract;
/// the one argument here is either a null, a timezone, or the call's own
/// failure — the ProcedureCallFailed class, 42001.
pub fn construct_clock(kind: ClockKind, mode: ClockMode, arg: Option<&Value>) -> Result<Value> {
    let reading = clock_reading(mode);
    match arg {
        None => Ok(Value::Temporal(clock_temporal(
            kind,
            reading.utc,
            default_zone(kind, reading),
        )?)),
        Some(Value::Null) => Ok(Value::Null),
        Some(Value::String(zone)) => {
            let resolved = offset_for_zone(zone, IanaAt::Utc(reading.utc))?;
            Ok(Value::Temporal(clock_temporal(
                kind,
                reading.utc,
                resolved,
            )?))
        }
        // Live reports the non-string timezone argument as the call's own
        // failure — ProcedureCallFailed under 42001 (probed 2026-11-20 on
        // 2026.10.1: date.realtime(123)).
        Some(_) => Err(Error::procedure_call(format!(
            "Invalid call signature: {}() takes a timezone STRING",
            clock_spelling(kind, mode)
        ))),
    }
}

thread_local! {
    static PINNED: std::cell::Cell<Option<ClockReading>> =
        const { std::cell::Cell::new(None) };
}

/// Pins `date()` / `datetime()` / `time()` to one instant for a statement,
/// restoring the previous pin when it drops.
pub struct QueryClockGuard {
    previous: Option<ClockReading>,
}

impl Default for QueryClockGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryClockGuard {
    pub fn new() -> Self {
        Self::install(ClockReading::wall_clock())
    }

    /// Installs `reading` for the guard's lifetime, keeping the previous pin
    /// so a nested guard restores the outer statement's instant.
    fn install(reading: ClockReading) -> Self {
        let previous = PINNED.with(|c| c.replace(Some(reading)));
        Self { previous }
    }
}

#[cfg(test)]
impl QueryClockGuard {
    /// Test seam: pins the statement clock to `utc` with host local offset
    /// `local_offset`.
    pub fn pinned_at(utc: NaiveDateTime, local_offset: i32) -> Self {
        Self::install(ClockReading::pinned(utc, local_offset))
    }
}

impl Drop for QueryClockGuard {
    fn drop(&mut self) {
        PINNED.with(|c| c.set(self.previous));
    }
}

/// The reading an unpinned clock read uses: the host's wall clock, sampled
/// at the call. Every statement entry installs a [`QueryClockGuard`] (the
/// executor's statement runner), so this is reachable only from a direct
/// core embedder that calls a constructor outside a statement — a named,
/// documented contract rather than a silent fallback inside the pin.
fn unpinned_reading() -> ClockReading {
    ClockReading::wall_clock()
}

fn pinned() -> ClockReading {
    PINNED.with(|c| c.get()).unwrap_or_else(unpinned_reading)
}

/// The observation `mode` names: the pinned statement instant for
/// [`ClockMode::Transaction`] and [`ClockMode::Statement`], the live wall
/// clock for [`ClockMode::Realtime`].
fn clock_reading(mode: ClockMode) -> ClockReading {
    match mode {
        ClockMode::Transaction | ClockMode::Statement => pinned(),
        ClockMode::Realtime => ClockReading::wall_clock(),
    }
}

/// The pinned clock's value of a kind — the production paths' shared body
/// (`construct` reads it directly; the per-kind `now_*` readers below are
/// test-only façades over it).
pub(super) fn now_value(kind: ClockKind) -> Temporal {
    let reading = pinned();
    clock_temporal(kind, reading.utc, default_zone(kind, reading))
        .expect("the host clock is representable")
}

#[cfg(test)]
pub(super) fn now_date() -> Temporal {
    now_value(ClockKind::Date)
}

#[cfg(test)]
pub(super) fn now_local_dt() -> Temporal {
    now_value(ClockKind::LocalDateTime)
}

#[cfg(test)]
pub(super) fn now_dt() -> Temporal {
    now_value(ClockKind::DateTime)
}

/// The pinned statement instant as milliseconds since 1970-01-01T00:00:00Z,
/// the same instant `now_dt()` reads (so `timestamp()` equals
/// `datetime().epochMillis` for the whole statement).
pub fn now_epoch_millis() -> i64 {
    pinned().utc.and_utc().timestamp_millis()
}

#[cfg(test)]
pub(super) fn now_local_time() -> Temporal {
    now_value(ClockKind::LocalTime)
}

#[cfg(test)]
pub(super) fn now_time() -> Temporal {
    now_value(ClockKind::Time)
}

/// The pinned statement instant's UTC wall clock — the `now_*` family's
/// naive-UTC member, for callers that resolve a zone at "now" without
/// constructing a `Temporal` (the `time` truncate boundary).
pub fn now_utc_dt() -> NaiveDateTime {
    pinned().utc
}
