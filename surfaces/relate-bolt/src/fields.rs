//! The Bolt request-field decoders (ADR-0342): the one extra-map accessor
//! family (`extra`, `extra_str`, `extra_int`, `extra_int_or`,
//! `patch_names`), the `db_name` home-database default, the RUN statement
//! requirement and the parsed RUN and PULL/DISCARD requests, plus the
//! 5.x HELLO `bolt_agent` shape and the Bolt 5.2 notifications filter
//! keys (ADR-0484). Every accessor returns the malformed-request message
//! instead of a fabricated default; the session's handlers own the
//! `Neo.ClientError.Request.Invalid` failure exits. Wire-pure: nothing
//! here reads or writes the stream.

use std::collections::BTreeMap;

use relate::Value;
use relate_model::wire::{Notification, NotificationSeverity};

use crate::dialect::Dialect;
use crate::packstream::Pack;
use crate::param_structs::params_to_map;

/// The home database: the namespace a RUN, BEGIN or ROUTE that names no
/// `db` selects (ADR-0117 §7, ADR-0318).
pub(crate) const HOME_DATABASE: &str = "neo4j";

/// The raw value of `key` in the extra map at `fields[index]`: `None` when
/// the field is absent, is not a map, or lacks the key. The one extra-map
/// lookup every accessor below builds on.
fn extra<'a>(fields: &'a [Pack], index: usize, key: &str) -> Option<&'a Pack> {
    fields.get(index).and_then(|extra| extra.get(key))
}

/// The string value of `key`: absent, `null`, not a string and the empty
/// string all mean `None` (ADR-0342).
pub(crate) fn extra_str<'a>(fields: &'a [Pack], index: usize, key: &str) -> Option<&'a str> {
    extra(fields, index, key)
        .and_then(Pack::as_str)
        .filter(|value| !value.is_empty())
}

/// The Int value of `key`: `None` only when the key is absent; a present
/// value of another type is a malformed request (ADR-0342).
fn extra_int(fields: &[Pack], index: usize, key: &str) -> Result<Option<i64>, String> {
    match extra(fields, index, key) {
        None => Ok(None),
        Some(value) => value.as_int().map(Some).ok_or_else(|| {
            format!(
                "unsupported extra value for {key}: {} (must be an Int)",
                value.kind_name()
            )
        }),
    }
}

/// The Int value of `key`, or `default` when the key is absent (ADR-0342):
/// the one place the `n`/`qid` `-1` default is applied. A present value of
/// another type is still a malformed request.
pub(crate) fn extra_int_or(
    fields: &[Pack],
    index: usize,
    key: &str,
    default: i64,
) -> Result<i64, String> {
    Ok(extra_int(fields, index, key)?.unwrap_or(default))
}

/// The HELLO extra's `patch_bolt` list (ADR-0329): the requested patch
/// names, in list order. A `patch_bolt` that is not a list contributes no
/// name, and only the string entries of a list are names; a non-string
/// entry negotiates nothing (as pinned by
/// `only_a_lists_string_entries_are_negotiated`). The names are the whole
/// vocabulary the negotiation sees; the Pack shape stays here, on the
/// codec's side of the edge.
pub(crate) fn patch_names(fields: &[Pack]) -> Vec<&str> {
    match extra(fields, 0, "patch_bolt") {
        Some(Pack::List(names)) => names.iter().filter_map(Pack::as_str).collect(),
        _ => Vec::new(),
    }
}

/// Whether the HELLO extra's `bolt_agent` carries the 5.3-required shape
/// (ADR-0484 decision 3): a map whose keys and values are all strings.
/// Absent, null or any other shape is `false`.
pub(crate) fn bolt_agent_is_string_map(fields: &[Pack]) -> bool {
    match extra(fields, 0, "bolt_agent") {
        Some(Pack::Map(entries)) => entries
            .iter()
            .all(|(key, value)| matches!((key, value), (Pack::String(_), Pack::String(_)))),
        _ => false,
    }
}

/// The `db` of a RUN, BEGIN or ROUTE extra map (ADR-0117 §7, ADR-0318):
/// the string value of the extra's `db` key at `fields[index]`, or the home
/// database when the key is absent, `null`, `""` or not a string. The one
/// place the home-database default is applied.
pub(crate) fn db_name(fields: &[Pack], index: usize) -> &str {
    extra_str(fields, index, "db").unwrap_or(HOME_DATABASE)
}

/// The RUN statement: `fields[0]` must be a string (ADR-0342). An absent
/// or non-string field is the same malformed-request failure as an
/// unrepresentable parameter; the message names what was found.
fn statement(fields: &[Pack]) -> Result<&str, String> {
    match fields.first() {
        Some(Pack::String(cypher)) => Ok(cypher),
        Some(value) => Err(format!(
            "unsupported request value for statement: {} (RUN statement must be a string)",
            value.kind_name()
        )),
        None => Err("missing RUN statement (RUN statement must be a string)".into()),
    }
}

/// A parsed RUN request: the statement and its parameter map (ADR-0342).
/// [`RunRequest::parse`] is the one decode of RUN's two fields, so both
/// malformed shapes — an absent/non-string statement and an unconvertible
/// params field — leave through its single `Err`.
pub(crate) struct RunRequest {
    pub(crate) cypher: String,
    pub(crate) params: BTreeMap<String, Value>,
}

impl RunRequest {
    /// The one decode of RUN's two fields: `dialect` is the connection's
    /// negotiated dialect (ADR-0329, ADR-0484), which selects the
    /// DateTime parameter pair (decision 7).
    pub(crate) fn parse(fields: &[Pack], dialect: Dialect) -> Result<Self, String> {
        // ADR-0342: the statement field is required — an absent or
        // non-string one is the same malformed-request failure as an
        // unrepresentable parameter, and nothing executes.
        let cypher = statement(fields)?.to_string();
        // ADR-0177: parameters the engine cannot represent fail the RUN
        // before namespace selection and before the engine call. An absent
        // params field stays the empty map.
        let params = match fields.get(1).map(|p| params_to_map(p, dialect)) {
            None => BTreeMap::new(),
            Some(Ok(params)) => params,
            Some(Err(message)) => return Err(message),
        };
        Ok(Self { cypher, params })
    }
}

/// A parsed PULL/DISCARD request: the addressed `qid` and the row cap
/// (ADR-0342). [`ConsumeRequest::parse`] is the one decode of the extra's
/// `qid` and `n` fields, so both malformed shapes — a non-Int value and an
/// `n` outside `-1` and the non-negative Ints — leave through its single
/// `Err`.
pub(crate) struct ConsumeRequest {
    /// The addressed result: `-1` (the absent-key default) is the most
    /// recent RUN's result.
    pub(crate) qid: i64,
    /// The row cap: `None` drains, `Some(n)` takes at most `n` rows.
    pub(crate) cap: Option<usize>,
}

impl ConsumeRequest {
    pub(crate) fn parse(fields: &[Pack]) -> Result<Self, String> {
        // ADR-0342: a present-but-malformed `qid` or `n` is a request
        // failure, exactly like an unknown `qid` at the session's take; the
        // `-1` defaults apply only when the key is absent.
        let qid = extra_int_or(fields, 0, "qid", -1)?;
        let n = extra_int_or(fields, 0, "n", -1)?;
        // ADR-0342: `-1` is the absent-key drain default, never a fallback
        // for a present out-of-range value; a present `n` outside `-1` and
        // the non-negative Ints fails before anything is taken. An `n`
        // above `usize::MAX` (possible on a 32-bit target) caps at
        // `usize::MAX`, so it still drains the rows left instead of
        // panicking on a target-width invariant.
        let cap = match n {
            -1 => None,
            n if n >= 0 => Some(usize::try_from(n).unwrap_or(usize::MAX)),
            n => {
                return Err(format!(
                    "unsupported extra value for n: {n} (must be -1 or a non-negative Int)"
                ))
            }
        };
        Ok(Self { qid, cap })
    }

    /// The rows this request takes out of `remaining`: the cap when
    /// present, the whole remainder when absent. The one home of the
    /// clamp, shared by the session's PULL/DISCARD step and its tests.
    pub(crate) fn take(&self, remaining: usize) -> usize {
        self.cap.map_or(remaining, |n| n.min(remaining))
    }
}

/// The `notifications_minimum_severity` value set (ADR-0484 decision 8):
/// the engine's two severities plus the protocol's `OFF`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MinimumSeverity {
    /// `INFORMATION`: every severity passes — the engine's two severities
    /// are both at least it.
    Information,
    /// `WARNING`: only `WARNING` notifications pass.
    Warning,
    /// `OFF`: no notification passes — the whole list drops.
    Off,
}

impl MinimumSeverity {
    /// The severity named on the wire, or `None` for a value outside the
    /// set — the exact spellings; Neo4j's wider value space is not
    /// offered.
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "INFORMATION" => Some(Self::Information),
            "WARNING" => Some(Self::Warning),
            "OFF" => Some(Self::Off),
            _ => None,
        }
    }
}

/// The Bolt 5.2 notifications filter keys (ADR-0484 decision 8): the
/// 5.4-era spellings — the 5.6 rename is not offered.
pub(crate) const NOTIFICATIONS_MINIMUM_SEVERITY: &str = "notifications_minimum_severity";
pub(crate) const NOTIFICATIONS_DISABLED_CATEGORIES: &str = "notifications_disabled_categories";

/// Neo4j's NotificationConfig parse failures (probed for the severity,
/// ADR-0484 decision 8): the text its `Illegal value for field "…"`
/// wrapper quotes as the field name.
const SEVERITY_PARSE_FAILURE: &str = "Could not parse a NotificationConfig's minimum severity.";
const CATEGORIES_PARSE_FAILURE: &str =
    "Could not parse a NotificationConfig's disabled categories.";

/// The notifications filter a statement's ending summary runs under
/// (ADR-0484 decision 8): the Bolt 5.2 keys `notifications_minimum_severity`
/// and `notifications_disabled_categories`, accepted at 5.4 in the HELLO,
/// BEGIN and RUN extras. Both keys are unset by default; a HELLO's values
/// are the connection's base, a BEGIN's overlay them for the transaction,
/// and a RUN's overlay whatever base its statement runs under — each unset
/// or null key falling back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NotificationConfig {
    /// `notifications_minimum_severity`: `None` when the key was absent or
    /// null. `Some(Information)` filters nothing, like `None`.
    minimum_severity: Option<MinimumSeverity>,
    /// `notifications_disabled_categories`: `None` when the key was absent
    /// or null; `Some(vec![])` is an explicit empty list — no category
    /// disabled.
    disabled_categories: Option<Vec<String>>,
}

impl NotificationConfig {
    /// Parse the two keys from the extra map at `fields[index]`: an absent
    /// or null key stays unset, a value outside the severity set or a
    /// wrong-typed value is `Err` with the parse failure Neo4j's wrapper
    /// quotes. Category entries are free strings — the engine's categories
    /// and Neo4j's wider set are both accepted, the latter filtering
    /// nothing.
    pub(crate) fn parse(fields: &[Pack], index: usize) -> Result<Self, String> {
        let minimum_severity = match extra(fields, index, NOTIFICATIONS_MINIMUM_SEVERITY) {
            None | Some(Pack::Null) => None,
            Some(Pack::String(value)) => {
                Some(MinimumSeverity::from_wire(value).ok_or(SEVERITY_PARSE_FAILURE)?)
            }
            Some(_) => return Err(SEVERITY_PARSE_FAILURE.into()),
        };
        let disabled_categories = match extra(fields, index, NOTIFICATIONS_DISABLED_CATEGORIES) {
            None | Some(Pack::Null) => None,
            Some(Pack::List(entries)) => {
                let mut categories = Vec::with_capacity(entries.len());
                for entry in entries {
                    let Some(name) = entry.as_str() else {
                        return Err(CATEGORIES_PARSE_FAILURE.into());
                    };
                    categories.push(name.to_string());
                }
                Some(categories)
            }
            Some(_) => return Err(CATEGORIES_PARSE_FAILURE.into()),
        };
        Ok(Self {
            minimum_severity,
            disabled_categories,
        })
    }

    /// The config a statement runs under: every key the statement's extra
    /// set overrides `base`'s, every key it left unset or null falls back
    /// to it (ADR-0484 decision 8).
    pub(crate) fn overlay(statement: Self, base: &Self) -> Self {
        Self {
            minimum_severity: statement.minimum_severity.or(base.minimum_severity),
            disabled_categories: statement
                .disabled_categories
                .or_else(|| base.disabled_categories.clone()),
        }
    }

    /// Whether `notification` passes this filter: its severity meets the
    /// minimum — `OFF` drops everything — and its category is not
    /// disabled. A disabled category is a free string matched exactly; one
    /// naming no engine category filters nothing.
    pub(crate) fn keeps(&self, notification: &Notification) -> bool {
        let severity_passes = match self.minimum_severity {
            Some(MinimumSeverity::Off) => false,
            Some(MinimumSeverity::Warning) => {
                notification.severity() == NotificationSeverity::Warning
            }
            Some(MinimumSeverity::Information) | None => true,
        };
        let category = notification.category().as_str();
        severity_passes
            && !self
                .disabled_categories
                .as_ref()
                .is_some_and(|categories| categories.iter().any(|c| c.as_str() == category))
    }
}

/// Neo4j's probed `Illegal value for field "…"` wrapper (ADR-0484
/// decision 8): the one template the notification-key rejections build
/// on — the HELLO extra's rejection directly, the RUN/BEGIN extra's
/// nested under the `metadata` field name.
pub(crate) fn illegal_value_for_field(name: &str) -> String {
    format!("Illegal value for field \"{name}\"")
}

/// The same rejection in a RUN or BEGIN extra (probed for RUN): nested
/// under the extra's `metadata` field name.
pub(crate) fn metadata_notifications_failure(parse_failure: &str) -> String {
    format!(
        "{}: {}",
        illegal_value_for_field("metadata"),
        illegal_value_for_field(parse_failure)
    )
}

#[cfg(test)]
#[path = "fields_tests.rs"]
mod tests;
