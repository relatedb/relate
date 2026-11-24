//! The plan's row vocabulary (ADR-0401 §2): the one row shape every
//! entity kind carries, and the identity-column layout the pre-read and
//! the row identity encoder share.

use std::collections::BTreeMap;

use relate::Value;

use crate::LoadError;

/// One planned row, pre-classification (ADR-0401 §2): the property
/// payload plus whichever key-carrying maps its entity kind has — the
/// merge key a keyed node or rel is matched by, the locally resolved
/// endpoint keys of a rel. The constructors are the four shapes; each
/// group's rows all come from one of them.
#[derive(Debug, Clone)]
pub(crate) struct PlanRow {
    start: Option<BTreeMap<String, Value>>,
    end: Option<BTreeMap<String, Value>>,
    key: Option<BTreeMap<String, Value>>,
    pub props: BTreeMap<String, Value>,
}

impl PlanRow {
    /// A keyed node row: the merge-key properties the remote row is
    /// matched by, plus the payload.
    pub(crate) fn keyed_node(key: BTreeMap<String, Value>, props: BTreeMap<String, Value>) -> Self {
        PlanRow {
            start: None,
            end: None,
            key: Some(key),
            props,
        }
    }

    /// An unkeyed `CREATE` node row: payload only — it has no identity to
    /// match a remote row by.
    pub(crate) fn created_node(props: BTreeMap<String, Value>) -> Self {
        PlanRow {
            start: None,
            end: None,
            key: None,
            props,
        }
    }

    /// A rel row written under a resolved relationship key.
    pub(crate) fn keyed_rel(
        start: BTreeMap<String, Value>,
        end: BTreeMap<String, Value>,
        key: BTreeMap<String, Value>,
        props: BTreeMap<String, Value>,
    ) -> Self {
        PlanRow {
            start: Some(start),
            end: Some(end),
            key: Some(key),
            props,
        }
    }

    /// A rel row in every other mode (structural, CREATE, or the keyed
    /// mode with no resolved key): matched by endpoints only.
    pub(crate) fn pair_rel(
        start: BTreeMap<String, Value>,
        end: BTreeMap<String, Value>,
        props: BTreeMap<String, Value>,
    ) -> Self {
        PlanRow {
            start: Some(start),
            end: Some(end),
            key: None,
            props,
        }
    }

    /// The row's key-carrying map for one identity field, when its shape
    /// has that field.
    pub(crate) fn identity_map(&self, field: IdentityField) -> Option<&BTreeMap<String, Value>> {
        match field {
            IdentityField::Key => self.key.as_ref(),
            IdentityField::Start => self.start.as_ref(),
            IdentityField::End => self.end.as_ref(),
        }
    }
}

/// Which key-carrying map of a row an identity column reads.
#[derive(Debug, Clone, Copy)]
pub(crate) enum IdentityField {
    Key,
    Start,
    End,
}

/// One run of identity columns: every property of `props`, read from one
/// row field and selected under one pre-read variable and alias prefix.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IdentityRun<'a> {
    pub field: IdentityField,
    pub var: &'static str,
    pub alias: &'static str,
    pub props: &'a [String],
}

/// A keyed group's identity-column derivation (start keys, end keys, rel
/// keys, in that order) — the ONE source of the pre-read's RETURN items,
/// its cell boundary, and the row identity encoder, so the remote side
/// and the local side cannot disagree. Unkeyed kinds have none.
#[derive(Debug, Clone)]
pub(crate) struct IdentityLayout<'a> {
    pub runs: Vec<IdentityRun<'a>>,
    /// The pre-read variable the `--updated-at` candidate columns read.
    pub candidate_var: &'static str,
}

impl IdentityLayout<'_> {
    /// The number of identity columns.
    pub(crate) fn column_count(&self) -> usize {
        self.runs.iter().map(|run| run.props.len()).sum()
    }

    /// The identity values of one row, in column order, read through the
    /// canonical keyed extraction.
    pub(crate) fn row_values<'r>(
        &self,
        entity: &str,
        row: &'r PlanRow,
    ) -> Result<Vec<&'r Value>, LoadError> {
        let mut values = Vec::with_capacity(self.column_count());
        for run in &self.runs {
            if run.props.is_empty() {
                continue;
            }
            let map = row.identity_map(run.field).ok_or_else(|| {
                LoadError::Push(format!(
                    "entity {entity}: a row shape without the {:?} identity field",
                    run.field
                ))
            })?;
            values.extend(keyed_values(entity, map, run.props)?);
        }
        Ok(values)
    }
}

/// The values of one element's key properties, read in the merge key's
/// order — the one keyed extraction: [`keyed_props`] (the row's key map)
/// and the identity encoder both consume it. A missing key property
/// fails closed naming the entity and the property (a merge key the data
/// does not satisfy is a local integrity error, not a silent partial
/// push).
pub(super) fn keyed_values<'a>(
    entity: &str,
    props: &'a BTreeMap<String, Value>,
    key: &[String],
) -> Result<Vec<&'a Value>, LoadError> {
    key.iter()
        .map(|prop| {
            props
                .get(prop)
                .ok_or_else(|| missing_key_prop(entity, prop))
        })
        .collect()
}

/// The keyed properties of one element as a map, in the merge key's order.
pub(super) fn keyed_props(
    entity: &str,
    props: &BTreeMap<String, Value>,
    key: &[String],
) -> Result<BTreeMap<String, Value>, LoadError> {
    let values = keyed_values(entity, props, key)?;
    Ok(key
        .iter()
        .cloned()
        .zip(values.into_iter().cloned())
        .collect())
}

/// The merge-key-missing refusal, naming the entity and the property.
fn missing_key_prop(entity: &str, prop: &str) -> LoadError {
    LoadError::Push(format!(
        "merge-key property {prop:?} is missing on a {entity} entity in the local scope"
    ))
}
