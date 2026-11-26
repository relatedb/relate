//! [`Temporal`] — the result type for a temporal instant a stdlib type cannot
//! carry losslessly — and [`Duration`], the one result type of every
//! duration.
//!
//! `convert` builds these; the pymodule registers the classes from the crate
//! root. Every date/time/datetime result is still returned as its stdlib
//! counterpart (`datetime.datetime` / `datetime.date` / `datetime.time`) when
//! that is lossless, so DB-API consumers are unaffected. [`Temporal`] appears
//! only for the two losses stdlib types cannot avoid: sub-microsecond
//! nanoseconds (stdlib temporals hold whole microseconds) and an IANA zone
//! name (stdlib holds a fixed offset only).
//!
//! A duration is always a [`Duration`] (ADR-0502 §1): `datetime.timedelta`
//! normalizes hours into days (`PT36H` would equal `P1DT12H`, which Cypher
//! keeps apart), has no month component and holds whole microseconds, so no
//! duration crosses as a `timedelta` — [`Duration::to_timedelta`] is the
//! explicit, checked projection.

use chrono::{DateTime, FixedOffset, NaiveDateTime, NaiveTime, Timelike};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use relate_model::value::IsoDuration;

/// The payload of a [`Temporal`]: exactly the temporal kinds whose stdlib
/// counterpart is lossy. A `Date` never is (no fractional second, no zone),
/// and a duration has its own lossless class ([`Duration`]), so neither needs
/// a variant.
pub(super) enum TemporalParts {
    /// A zoned instant: the local wall clock (the offset is already applied)
    /// plus the IANA zone name the engine kept, when it kept one.
    DateTime {
        local: DateTime<FixedOffset>,
        zone: Option<String>,
    },
    /// A wall clock with no zone and no offset.
    LocalDateTime(NaiveDateTime),
    /// A time of day with an optional fixed offset.
    Time {
        time: NaiveTime,
        offset: Option<FixedOffset>,
    },
}

/// A temporal result that carries more than its stdlib counterpart can.
///
/// Returned instead of a stdlib `datetime.datetime` or `datetime.time` when the
/// value has sub-microsecond nanoseconds or an IANA zone name. `text` is the
/// engine's own canonical ISO-8601 rendering (ADR-0448) — `str()`, `repr()` and
/// equality all use it, so a value reproduces the engine's output exactly. The
/// getters expose the pieces the stdlib type drops:
/// [`nanosecond`](Self::nanosecond), [`zone_name`](Self::zone_name) and
/// [`offset_seconds`](Self::offset_seconds); every other field (year, hour, …)
/// is a [`to_native`](Self::to_native) call away.
#[pyclass(module = "relate._native")]
pub(super) struct Temporal {
    parts: TemporalParts,
    /// The engine's kind vocabulary — `Temporal::kind_name`, passed in by
    /// `convert` so the binding never re-spells it.
    kind: &'static str,
    text: String,
}

impl Temporal {
    pub(super) fn new(parts: TemporalParts, kind: &'static str, text: String) -> Self {
        Self { parts, kind, text }
    }
}

#[pymethods]
impl Temporal {
    /// The engine's canonical ISO-8601 text — also what `str()` returns and
    /// what equality compares.
    #[getter]
    fn text(&self) -> &str {
        &self.text
    }

    /// The engine's kind name: `"datetime"`, `"localdatetime"` or `"time"`.
    #[getter]
    fn kind(&self) -> &'static str {
        self.kind
    }

    /// Sub-second nanoseconds (0..999_999_999) — the precision stdlib cannot
    /// carry.
    #[getter]
    fn nanosecond(&self) -> u32 {
        match &self.parts {
            TemporalParts::DateTime { local, .. } => local.nanosecond(),
            TemporalParts::LocalDateTime(dt) => dt.nanosecond(),
            TemporalParts::Time { time, .. } => time.nanosecond(),
        }
    }

    /// The UTC offset in seconds, or `None` for an offset-free (local) value.
    #[getter]
    fn offset_seconds(&self) -> Option<i32> {
        match &self.parts {
            TemporalParts::DateTime { local, .. } => Some(local.offset().local_minus_utc()),
            TemporalParts::LocalDateTime(_) => None,
            TemporalParts::Time { offset, .. } => offset.map(|o| o.local_minus_utc()),
        }
    }

    /// The IANA zone name the engine kept (e.g. `"Europe/Stockholm"`), or
    /// `None` when the value has only an offset. The engine's classifier
    /// answers, so an offset-only spelling (`Z`, `UTC`) never surfaces here as
    /// a name.
    #[getter]
    fn zone_name(&self) -> Option<&str> {
        match &self.parts {
            TemporalParts::DateTime { zone, .. } => {
                relate_core::temporal::zone_region(zone.as_deref())
            }
            TemporalParts::LocalDateTime(_) | TemporalParts::Time { .. } => None,
        }
    }

    /// The stdlib value, floored to whole microseconds and — for a named zone
    /// — reduced to its fixed UTC offset. This is exactly the lossy projection
    /// [`Temporal`] exists to avoid; it is here so the value still interoperates
    /// with code that expects stdlib temporals.
    fn to_native(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match &self.parts {
            TemporalParts::DateTime { local, .. } => crate::convert::datetime_to_py(py, local),
            TemporalParts::LocalDateTime(dt) => crate::convert::localdatetime_to_py(py, dt),
            TemporalParts::Time { time, offset } => crate::convert::time_to_py(py, time, offset),
        }
    }

    fn __str__(&self) -> &str {
        &self.text
    }

    fn __repr__(&self) -> String {
        format!("Temporal(kind={:?}, text={:?})", self.kind, self.text)
    }

    /// Equality is the engine's: the canonical text carries the instant, the
    /// offset and the zone identity, which is exactly the `=` relation for
    /// these kinds (a named zone never equals the offset it denotes, while
    /// `Z` equals `+00:00` — both render `Z`).
    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        match other.cast::<Temporal>() {
            Ok(o) => o.borrow().text == self.text,
            Err(_) => false,
        }
    }

    fn __hash__(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.text.hash(&mut hasher);
        hasher.finish()
    }
}

const NANOS_PER_SECOND: i128 = 1_000_000_000;
const MICROS_PER_DAY: i128 = 86_400_000_000;

/// A Cypher duration, carried losslessly as the engine's four components
/// (ADR-0502 §1): `months`, `days`, `seconds` and `nanoseconds`.
///
/// Returned for every duration result, at every depth (inside lists, maps
/// and entity properties included), and accepted as a query parameter —
/// month-bearing and sub-microsecond durations bind without loss. The
/// components are kept apart exactly as Cypher keeps them: `PT36H`
/// (`seconds = 129600`) and `P1DT12H` (`days = 1, seconds = 43200`) are
/// different values. Equality and hashing follow the engine's `=`: months
/// and days compare as-is, `seconds` and `nanoseconds` as their combined
/// total, so `Duration(seconds=1, nanoseconds=500_000_000)` equals
/// `Duration(nanoseconds=1_500_000_000)`. `str()` is the engine's own
/// ISO-8601 rendering.
#[pyclass(module = "relate._native", frozen)]
#[derive(Clone)]
pub(super) struct Duration {
    pub(super) value: IsoDuration,
}

impl Duration {
    /// The `(months, days, total nanoseconds)` triple equality and hashing
    /// read — the engine's own representation of a duration
    /// (`Temporal::Duration { months, days, nanos }`), widened to `i128` so
    /// no `i64` component pair overflows.
    fn identity(&self) -> (i64, i64, i128) {
        (
            self.value.months,
            self.value.days,
            i128::from(self.value.seconds) * NANOS_PER_SECOND + i128::from(self.value.nanoseconds),
        )
    }

    /// The `(days, seconds, microseconds)` triple `datetime.timedelta`
    /// expects, or `None` when no lossless `timedelta` exists — the duration
    /// has months, sub-microsecond nanoseconds (stdlib holds whole
    /// microseconds), or lies outside `timedelta`'s range (`timedelta.min` is
    /// -999_999_999 days exactly; `timedelta.max` is 999_999_999 days plus
    /// 23:59:59.999999).
    ///
    /// The engine keeps `seconds` and `nanoseconds` unnormalized
    /// (`PT2400000000S` carries 2_400_000_000 seconds); passing those raw to
    /// `timedelta` makes CPython reject the argument with an opaque "Python
    /// int too large to convert to C int", so the split happens here.
    fn timedelta_parts(&self) -> Option<(i128, i128, i128)> {
        let dur = &self.value;
        if dur.months != 0 || dur.nanoseconds % 1_000 != 0 {
            return None;
        }
        let total_micros = (i128::from(dur.days) * 86_400 + i128::from(dur.seconds)) * 1_000_000
            + i128::from(dur.nanoseconds) / 1_000;
        let days = total_micros.div_euclid(MICROS_PER_DAY);
        if !(-999_999_999..=999_999_999).contains(&days) {
            return None;
        }
        // The range check bounds each part: days ±999_999_999, seconds
        // 0..86_399, microseconds 0..999_999 — all within `timedelta`'s
        // argument range.
        let within_day = total_micros.rem_euclid(MICROS_PER_DAY);
        Some((days, within_day / 1_000_000, within_day % 1_000_000))
    }
}

#[pymethods]
impl Duration {
    #[new]
    #[pyo3(signature = (months = 0, days = 0, seconds = 0, nanoseconds = 0))]
    fn new(months: i64, days: i64, seconds: i64, nanoseconds: i64) -> Self {
        Duration {
            value: IsoDuration::new(months, days, seconds, nanoseconds),
        }
    }

    /// Whole months (years are twelve months each).
    #[getter]
    fn months(&self) -> i64 {
        self.value.months
    }

    /// Whole days, kept apart from `seconds` (a day is not always 24 hours).
    #[getter]
    fn days(&self) -> i64 {
        self.value.days
    }

    /// Seconds, kept apart from `days` (`PT36H` is 129600 seconds).
    #[getter]
    fn seconds(&self) -> i64 {
        self.value.seconds
    }

    /// The sub-second nanoseconds, as the engine carries them.
    #[getter]
    fn nanoseconds(&self) -> i64 {
        self.value.nanoseconds
    }

    /// The equivalent `datetime.timedelta` — the explicit, checked lossy
    /// projection. Raises `ValueError` when the duration has months, has
    /// sub-microsecond nanoseconds, or lies outside `timedelta`'s range;
    /// note that `timedelta` normalizes seconds into days, so two different
    /// durations (`PT36H`, `P1DT12H`) can project to equal `timedelta`s.
    fn to_timedelta(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let (days, seconds, micros) = self.timedelta_parts().ok_or_else(|| {
            PyValueError::new_err(format!(
                "duration {} has no lossless datetime.timedelta: it has months, \
                 sub-microsecond nanoseconds, or lies outside timedelta's range",
                self.value
            ))
        })?;
        Ok(py
            .import("datetime")?
            .call_method1("timedelta", (days, seconds, micros))?
            .unbind())
    }

    fn __str__(&self) -> String {
        self.value.to_string()
    }

    fn __repr__(&self) -> String {
        format!(
            "Duration(months={}, days={}, seconds={}, nanoseconds={})",
            self.value.months, self.value.days, self.value.seconds, self.value.nanoseconds
        )
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        match other.cast::<Duration>() {
            Ok(o) => o.get().identity() == self.identity(),
            Err(_) => false,
        }
    }

    fn __hash__(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.identity().hash(&mut hasher);
        hasher.finish()
    }
}
