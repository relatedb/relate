//! Push policies (ADR-0401 §3/§4/§5): update policies (global or
//! per-entity), the relationship merge mode, the unkeyed-label action, and
//! the `--updated-at` conflict decision — the pure, flag-value half of the
//! push, unit-tested without a store or a network.

use std::collections::BTreeMap;

use relate::Value;

/// One entity's update policy (ADR-0401 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdatePolicy {
    /// `SET +=` — local properties land, remote-only properties survive
    /// (the default, ADR-0401 §3: one carrier for both of its homes — a
    /// push with no flag and a policy list with no bare global word).
    #[default]
    Add,
    /// `SET =` — local truth overwrites the property map; remote-only
    /// properties on that entity are removed.
    Replace,
    /// Only missing entities are created; existing ones untouched.
    Skip,
}

impl UpdatePolicy {
    /// The flag spelling, also the report's policy column.
    pub fn as_str(self) -> &'static str {
        match self {
            UpdatePolicy::Add => "add",
            UpdatePolicy::Replace => "replace",
            UpdatePolicy::Skip => "skip",
        }
    }

    /// Parse one policy word (ADR-0401 §3). A bad word is the caller's
    /// usage error, never a silent default.
    fn parse_word(word: &str) -> Result<Self, String> {
        match word {
            "add" => Ok(UpdatePolicy::Add),
            "replace" => Ok(UpdatePolicy::Replace),
            "skip" => Ok(UpdatePolicy::Skip),
            other => Err(format!(
                "unknown update policy {other:?} (expected add, replace or skip)"
            )),
        }
    }

    /// The policy's own SET item for variable `v`, without the keyword —
    /// the policy's whole say in the clause: which assignment it picks.
    /// Statement text (the `SET` keyword, the joining, any other items)
    /// is assembled where it belongs, in statement.rs.
    pub(crate) fn set_item(self, v: &str) -> String {
        match self {
            UpdatePolicy::Add => format!("{v} += row.props"),
            // `=` for both: `replace` overwrites the property map, and
            // `skip` only ever carries to-create rows, where `=` on an
            // empty node states outright that nothing existing is
            // touched.
            UpdatePolicy::Replace | UpdatePolicy::Skip => format!("{v} = row.props"),
        }
    }
}

/// A parsed `--nodes-update` / `--rels-update` value (ADR-0401 §3): a bare
/// global policy, a per-entity list (`Movie=replace,Person=skip`), or a
/// global policy plus named per-entity overrides (`replace,Movie=skip`).
/// The bare global is the default for every entity not named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySpec {
    global: UpdatePolicy,
    per_entity: BTreeMap<String, UpdatePolicy>,
}

impl PolicySpec {
    /// The all-`add` default (ADR-0401 §3).
    pub fn default_spec() -> Self {
        Self {
            global: UpdatePolicy::default(),
            per_entity: BTreeMap::new(),
        }
    }

    /// Parse a policy flag value. Comma-separated items; an item is either
    /// a bare policy word (the global; at most one) or `Entity=policy`. A
    /// second global, an empty entity name, or an unknown policy word is a
    /// usage error.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut global: Option<UpdatePolicy> = None;
        let mut per_entity = BTreeMap::new();
        for item in spec.split(',') {
            let item = item.trim();
            if item.is_empty() {
                return Err("empty item in policy list".to_string());
            }
            match item.split_once('=') {
                None => {
                    let policy = UpdatePolicy::parse_word(item)?;
                    if let Some(existing) = global.as_ref() {
                        return Err(format!(
                            "policy list has two global policies ({:?} twice)",
                            existing.as_str()
                        ));
                    }
                    global = Some(policy);
                }
                Some((entity, word)) => {
                    let entity = entity.trim();
                    if entity.is_empty() {
                        return Err(format!("empty entity name in policy item {item:?}"));
                    }
                    let policy = UpdatePolicy::parse_word(word.trim())?;
                    if per_entity.insert(entity.to_string(), policy).is_some() {
                        // A second policy for one entity is a usage error,
                        // never a silently-overriding winner.
                        return Err(format!("policy list names entity {entity:?} twice"));
                    }
                }
            }
        }
        Ok(Self {
            // The ADR's default when no bare global was named.
            global: global.unwrap_or_default(),
            per_entity,
        })
    }

    /// The policy for `entity`: a named override if present, else the
    /// global. The one resolution rule, shared by nodes and rels.
    pub fn resolve(&self, entity: &str) -> UpdatePolicy {
        self.per_entity.get(entity).copied().unwrap_or(self.global)
    }
}

/// Relationship merge mode (ADR-0401 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelMode {
    /// Merge on the type's resolved key when one exists — a
    /// `--rel-key TYPE:PROP[,PROP]` override, else the type's
    /// relationship uniqueness/key constraint (ADR-0401 §5) — and fall
    /// back to one relationship of a type between two nodes when neither
    /// exists. The default: identity is resolved, not mode-selected.
    Keyed,
    /// `MERGE (a)-[:T]->(b)` — one relationship of a type between two
    /// nodes, idempotent across re-pushes; the explicit force that
    /// ignores any resolved key (parallel rels collapse).
    Structural,
    /// `CREATE` — the explicit non-idempotent escape hatch.
    Create,
}

impl RelMode {
    /// Parse `--rel-mode`'s value; a bad word is the caller's usage error.
    pub fn parse(word: &str) -> Result<Self, String> {
        match word {
            "keyed" => Ok(RelMode::Keyed),
            "structural" => Ok(RelMode::Structural),
            "create" => Ok(RelMode::Create),
            other => Err(format!(
                "unknown --rel-mode {other:?} (expected keyed, structural or create)"
            )),
        }
    }
}

/// What happens to a label with no merge key (ADR-0401 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnkeyedAction {
    /// Report and skip, counted in the run report — the fail-closed default.
    Skip,
    /// `CREATE` rows instead: one-shot seeding semantics, documented as
    /// non-idempotent.
    Create,
}

impl UnkeyedAction {
    /// Parse `--unkeyed-labels`' value; a bad word is the caller's usage
    /// error.
    pub fn parse(word: &str) -> Result<Self, String> {
        match word {
            "skip" => Ok(UnkeyedAction::Skip),
            "create" => Ok(UnkeyedAction::Create),
            other => Err(format!(
                "unknown --unkeyed-labels {other:?} (expected skip or create)"
            )),
        }
    }
}

/// Parse a `--merge-key LABEL:PROP[,PROP]` / `--rel-key TYPE:PROP[,PROP]`
/// value (ADR-0401 §1/§5): an explicit key for one label or relationship
/// type, overriding the constraint-derived key. The name may not be empty
/// and the property list may not be empty.
pub fn parse_key_spec(flag: &str, spec: &str) -> Result<(String, Vec<String>), String> {
    let Some((label, props)) = spec.split_once(':') else {
        return Err(format!(
            "{flag} {spec:?} must be NAME:PROP[,PROP] (no ':' found)"
        ));
    };
    let label = label.trim();
    if label.is_empty() {
        return Err(format!("{flag} {spec:?} has an empty name"));
    }
    let props: Vec<String> = props
        .split(',')
        .map(str::trim)
        .map(str::to_string)
        .collect();
    // `split(',')` on any string yields at least one item, so the
    // empty-name check above is also the no-properties check.
    if props.iter().any(|p| p.is_empty()) {
        return Err(format!("{flag} {spec:?} has an empty property name"));
    }
    Ok((label.to_string(), props))
}

/// Parse a `--updated-at NAME[,NAME]` value (ADR-0401 §4): the candidate
/// property names, in preference order (first present wins). Empty names
/// and duplicates are usage errors — the candidate list is explicit and
/// opt-in, never guessed.
pub fn parse_updated_at_spec(spec: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for name in spec.split(',') {
        let name = name.trim();
        if name.is_empty() {
            return Err(format!("--updated-at {spec:?} has an empty property name"));
        }
        if names.iter().any(|n| n == name) {
            return Err(format!("--updated-at {spec:?} names {name:?} twice"));
        }
        names.push(name.to_string());
    }
    // `split(',')` on any string yields at least one item, so the
    // empty-name check above is also the no-names check.
    Ok(names)
}

/// The `--updated-at` decision for one entity where both sides carry a
/// candidate (ADR-0401 §4): apply when the local value is newer or **equal**
/// (equal applies — idempotent re-push), skip when the remote is strictly
/// newer, warn-and-apply when the types are incomparable. Numbers compare
/// numerically across INTEGER/FLOAT; temporals compare through the engine's
/// own partial order; anything else is incomparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConflictDecision {
    Apply,
    RemoteNewer,
    Incomparable,
}

pub(crate) fn updated_at_decision(local: &Value, remote: &Value) -> ConflictDecision {
    use std::cmp::Ordering;
    match (local, remote) {
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let ge = match (local, remote) {
                (Value::Int(a), Value::Int(b)) => a >= b,
                _ => local
                    .as_number()
                    .zip(remote.as_number())
                    .is_some_and(|(a, b)| a >= b),
            };
            if ge {
                ConflictDecision::Apply
            } else {
                ConflictDecision::RemoteNewer
            }
        }
        (Value::Temporal(a), Value::Temporal(b)) => match a.compare(b) {
            Some(Ordering::Greater) | Some(Ordering::Equal) => ConflictDecision::Apply,
            Some(Ordering::Less) => ConflictDecision::RemoteNewer,
            None => ConflictDecision::Incomparable,
        },
        _ => ConflictDecision::Incomparable,
    }
}

/// Resolve an entity's `--updated-at` candidate (ADR-0401 §4): the first
/// candidate property the entity carries, in the flag's order. `None`
/// when the entity carries none — no conflict to decide.
pub(crate) fn resolve_candidate<'a>(
    props: &'a BTreeMap<String, Value>,
    candidates: &[String],
) -> Option<&'a Value> {
    candidates.iter().find_map(|name| props.get(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_spec_parses_global_form() {
        for (word, policy) in [
            ("add", UpdatePolicy::Add),
            ("replace", UpdatePolicy::Replace),
            ("skip", UpdatePolicy::Skip),
        ] {
            let spec = PolicySpec::parse(word).unwrap();
            assert_eq!(spec.resolve("Movie"), policy, "{word}");
            assert_eq!(spec.resolve("Person"), policy, "{word}");
        }
    }

    #[test]
    fn policy_spec_parses_per_entity_form() {
        // The ADR's example: named entities, everything else the add default.
        let spec = PolicySpec::parse("Movie=replace,Person=skip").unwrap();
        assert_eq!(spec.resolve("Movie"), UpdatePolicy::Replace);
        assert_eq!(spec.resolve("Person"), UpdatePolicy::Skip);
        assert_eq!(spec.resolve("Studio"), UpdatePolicy::Add);
    }

    #[test]
    fn policy_spec_parses_global_plus_overrides() {
        let spec = PolicySpec::parse("replace,Movie=skip").unwrap();
        assert_eq!(spec.resolve("Movie"), UpdatePolicy::Skip);
        assert_eq!(spec.resolve("Person"), UpdatePolicy::Replace);
    }

    #[test]
    fn policy_spec_rejects_bad_values() {
        for bad in [
            "wipe",
            "Movie=wipe",
            "add,replace",
            "Movie=replace,Movie=skip",
            "",
            "Movie=",
            "=skip",
        ] {
            assert!(PolicySpec::parse(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn default_spec_is_add_everywhere() {
        let spec = PolicySpec::default_spec();
        assert_eq!(spec.resolve("anything"), UpdatePolicy::Add);
    }

    #[test]
    fn rel_mode_parses_and_rejects() {
        assert_eq!(RelMode::parse("keyed").unwrap(), RelMode::Keyed);
        assert_eq!(RelMode::parse("structural").unwrap(), RelMode::Structural);
        assert_eq!(RelMode::parse("create").unwrap(), RelMode::Create);
        assert!(RelMode::parse("merge").is_err());
        assert!(RelMode::parse("unique").is_err());
    }

    #[test]
    fn unkeyed_action_parses_and_rejects() {
        assert_eq!(UnkeyedAction::parse("skip").unwrap(), UnkeyedAction::Skip);
        assert_eq!(
            UnkeyedAction::parse("create").unwrap(),
            UnkeyedAction::Create
        );
        assert!(UnkeyedAction::parse("insert").is_err());
    }

    #[test]
    fn key_spec_parses_composite_for_both_flags() {
        assert_eq!(
            parse_key_spec("--merge-key", "Movie:title").unwrap(),
            ("Movie".to_string(), vec!["title".to_string()])
        );
        assert_eq!(
            parse_key_spec("--rel-key", "ACTED_IN:slot").unwrap(),
            ("ACTED_IN".to_string(), vec!["slot".to_string()])
        );
        assert_eq!(
            parse_key_spec("--rel-key", "LEASED:tenant,name").unwrap(),
            (
                "LEASED".to_string(),
                vec!["tenant".to_string(), "name".to_string()]
            )
        );
        for bad in ["Movie", "Movie:", ":title", "Movie:,a", ""] {
            assert!(
                parse_key_spec("--merge-key", bad).is_err(),
                "{bad:?} parsed"
            );
            assert!(parse_key_spec("--rel-key", bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn updated_at_spec_parses_and_rejects() {
        assert_eq!(
            parse_updated_at_spec("updatedAt").unwrap(),
            vec!["updatedAt".to_string()]
        );
        assert_eq!(
            parse_updated_at_spec("updatedAt,updated,lastModified").unwrap(),
            vec![
                "updatedAt".to_string(),
                "updated".to_string(),
                "lastModified".to_string()
            ]
        );
        for bad in ["", "a,,b", "a,a"] {
            assert!(parse_updated_at_spec(bad).is_err(), "{bad:?} parsed");
        }
    }

    /// A local number that is newer or equal applies; a strictly newer
    /// remote is skipped; INTEGER and FLOAT compare across kinds.
    #[test]
    fn updated_at_numbers_compare_across_kinds() {
        let older = Value::Int(2);
        let same = Value::Float(2.0);
        let newer = Value::Int(3);
        assert_eq!(updated_at_decision(&newer, &older), ConflictDecision::Apply);
        assert_eq!(
            updated_at_decision(&older, &newer),
            ConflictDecision::RemoteNewer
        );
        // Equal applies — idempotent re-push — even across INTEGER/FLOAT.
        assert_eq!(updated_at_decision(&same, &older), ConflictDecision::Apply);
        assert_eq!(updated_at_decision(&older, &same), ConflictDecision::Apply);
    }

    /// Temporals compare through the engine's own order; a date against a
    /// string is incomparable (warn and apply).
    #[test]
    fn updated_at_temporals_and_incomparables() {
        fn date(s: &str) -> Value {
            Value::Temporal(
                relate_core::temporal::parse_iso("date", s)
                    .unwrap_or_else(|e| panic!("{s:?}: {e}")),
            )
        }
        let newer = date("2024-01-02");
        let older = date("2024-01-01");
        assert_eq!(updated_at_decision(&newer, &older), ConflictDecision::Apply);
        assert_eq!(
            updated_at_decision(&older, &newer),
            ConflictDecision::RemoteNewer
        );
        assert_eq!(updated_at_decision(&newer, &newer), ConflictDecision::Apply);
        // A date against a string is incomparable — warn and apply.
        assert_eq!(
            updated_at_decision(&newer, &Value::String("2024-01-01".into())),
            ConflictDecision::Incomparable
        );
    }

    #[test]
    fn set_item_spells_both_operators() {
        assert_eq!(UpdatePolicy::Add.set_item("n"), "n += row.props");
        assert_eq!(UpdatePolicy::Replace.set_item("r"), "r = row.props");
        assert_eq!(UpdatePolicy::Skip.set_item("n"), "n = row.props");
    }
}
