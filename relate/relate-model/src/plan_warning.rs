//! The typed plan-warning vocabulary (ADR-0344, typed node warnings;
//! LABS-461): the one warning type — the [`PlanWarning`] enum owning the
//! wire name, the message text, the payload variant, and the notification
//! projection once each — beside the estimate-vs-actual policy its
//! `EstimationMiss` variant derives from and the wire-shape serializer
//! the HTTP plan reads. It lives here, beside the notification vocabulary
//! it projects into and the wire parse it coordinates, so the engine's
//! detection, the wire parse, and every surface's rendering cannot drift
//! (not three review-coordinated layers).

use super::notification_wire::NotificationKind;
use super::plan_keys;

/// The typed warnings vocabulary's wire name, by variant — the one
/// spelling the engine's detection serializes into the wire form and
/// [`PlanWarning::from_wire_parts`] parses back; a name outside the
/// vocabulary is a contract break. In-crate today (the parse and the
/// model's tests); re-widened to `pub` in the same change as the first
/// cross-crate consumer naming a wire warning.
pub(crate) const WARNING_ESTIMATION_MISS: &str = "EstimationMiss";
/// See [`WARNING_ESTIMATION_MISS`].
pub(crate) const WARNING_CARTESIAN_PRODUCT: &str = "CartesianProduct";
/// See [`WARNING_ESTIMATION_MISS`].
pub(crate) const WARNING_ALL_NODES_SCAN: &str = "AllNodesScan";
/// See [`WARNING_ESTIMATION_MISS`].
pub(crate) const WARNING_LABEL_DISJUNCTION_SCAN: &str = "LabelDisjunctionScan";

/// The `EstimationMiss` overshoot factor (ADR-0344): the estimate misses
/// when it overshoots the actual rows by this many times or more — the
/// one spelling both the policy ([`estimation_miss`]) divides by and the
/// variant's message text names, so the factor and its "10x" cannot
/// drift apart.
const ESTIMATION_MISS_FACTOR: u64 = 10;

/// One typed warning a plan node carries (ADR-0344, typed node warnings):
/// the vocabulary's one definition — the wire name it serializes by, the
/// message text that renders beside the affected operator and in the
/// warnings footer, the payload variant that names the property index, and
/// the notification kind it projects to (ADR-0363 decision 1) — each owned
/// here once, beside the notification vocabulary it projects into, so the
/// engine's detection, the wire parse, and every surface's rendering
/// cannot drift (LABS-461: one warning type, not three coordinated
/// layers).
///
/// The engine constructs the variants at plan-tree construction (the
/// MATCH-shape detection in `relate`'s `plan_node/warning.rs`); the
/// wire accessor parses the `{name, index}` maps back into them — the one
/// typed vocabulary in both directions.
///
/// ```rust
/// # use relate_model::wire::PlanWarning;
/// let cartesian = PlanWarning::CartesianProduct;
/// assert_eq!(cartesian.name(), "CartesianProduct");
/// assert_eq!(
///     cartesian.message(),
///     "the MATCH patterns share no join anchor"
/// );
/// // The AllNodesScan warning names no index: a label-less pattern
/// // cannot use one, so the advice is the label.
/// let scan = PlanWarning::AllNodesScan;
/// assert_eq!(scan.name(), "AllNodesScan");
/// assert_eq!(
///     scan.message(),
///     "the scan matches every node; add a label to narrow it"
/// );
/// // The label-disjunction scan carries the per-label pairs its own
/// // predicate wants indexed (LABS-1083).
/// let disjunction = PlanWarning::LabelDisjunctionScan {
///     indexes: ":Model(id), :Source(id)".to_string(),
/// };
/// assert_eq!(disjunction.name(), "LabelDisjunctionScan");
/// assert_eq!(
///     disjunction.message(),
///     "the disjunction scan answers per label; consider the property index \
///      (:Model(id), :Source(id))"
/// );
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanWarning {
    /// The operator's estimate overshot its actual rows by 10x or more —
    /// the same policy the ratio cell's `!` marker reads. PROFILE only:
    /// it needs the actual rows, so it never fires on an EXPLAIN tree.
    EstimationMiss,
    /// A multi-pattern `MATCH` whose patterns share no join anchor (no
    /// node variable appears in more than one pattern) — the patterns
    /// multiply instead of joining.
    CartesianProduct,
    /// A label-less scan (`MATCH (n)`) while the store carries a property
    /// index. The warning names no index: no property index serves a
    /// pattern without a label, and the one the store carries may be
    /// unrelated to the query, so the advice is the label alone.
    AllNodesScan,
    /// A static label disjunction no seek serves (`MATCH (p) WHERE
    /// p:Model OR p:Source` after the planner's fold, LABS-1083): the
    /// scan answers per disjunct label, never every node — its own
    /// warning shape, never the `AllNodesScan` text. The warning carries
    /// the per-label `:Label(property)` pairs its equality predicate
    /// wants indexed, comma-joined, as the suggestion.
    LabelDisjunctionScan {
        /// The per-label index pairs the suggestion names.
        indexes: String,
    },
}

/// The payload-less variants, the one table the wire parse searches by
/// [`PlanWarning::name`] — a new payload-less variant is added here and
/// to [`PlanWarning::identity`]'s exhaustive match.
const PAYLOAD_LESS: [PlanWarning; 3] = [
    PlanWarning::EstimationMiss,
    PlanWarning::CartesianProduct,
    PlanWarning::AllNodesScan,
];

impl PlanWarning {
    /// One exhaustive variant mapping of the allocation-free half of the
    /// vocabulary: the wire name and the notification kind the warning
    /// projects to (ADR-0363 decision 1) — a fifth warning variant must
    /// choose both here rather than falling through a wildcard. Every
    /// projected kind is a plan warning in the model's partition
    /// ([`NotificationKind::is_plan_warning`]), so the plan table's
    /// warning text and the notification description are one spelling.
    fn identity(&self) -> (&'static str, NotificationKind) {
        match self {
            PlanWarning::EstimationMiss => {
                (WARNING_ESTIMATION_MISS, NotificationKind::EstimationMiss)
            }
            PlanWarning::CartesianProduct => (
                WARNING_CARTESIAN_PRODUCT,
                NotificationKind::CartesianProduct,
            ),
            PlanWarning::AllNodesScan => (WARNING_ALL_NODES_SCAN, NotificationKind::AllNodesScan),
            PlanWarning::LabelDisjunctionScan { .. } => (
                WARNING_LABEL_DISJUNCTION_SCAN,
                NotificationKind::LabelDisjunctionScan,
            ),
        }
    }

    /// The base message text, before any payload — one exhaustive match,
    /// built only where a message is rendered ([`Self::message`]).
    fn base_text(&self) -> String {
        match self {
            PlanWarning::EstimationMiss => {
                format!("estimated rows {ESTIMATION_MISS_FACTOR}x or more above the actual rows")
            }
            PlanWarning::CartesianProduct => "the MATCH patterns share no join anchor".to_string(),
            PlanWarning::AllNodesScan => {
                "the scan matches every node; add a label to narrow it".to_string()
            }
            PlanWarning::LabelDisjunctionScan { .. } => {
                "the disjunction scan answers per label; consider the property index".to_string()
            }
        }
    }

    /// The warning's wire name — the one spelling the wire form
    /// serializes (`warnings` on the node) and
    /// [`Self::from_wire_parts`] validates against.
    pub fn name(&self) -> &'static str {
        self.identity().0
    }

    /// The warning's index payload — the `LabelDisjunctionScan` warning's
    /// per-label index pairs, `None` on the payload-less variants. The one spelling
    /// of which warnings carry a payload, read by both the message text
    /// and the wire form's `index` field, so the vocabulary is matched once.
    pub fn suggested_index(&self) -> Option<&str> {
        match self {
            PlanWarning::LabelDisjunctionScan { indexes } => Some(indexes.as_str()),
            PlanWarning::EstimationMiss
            | PlanWarning::CartesianProduct
            | PlanWarning::AllNodesScan => None,
        }
    }

    /// The notification kind this warning projects to (ADR-0363
    /// decision 1) — from the one exhaustive variant mapping [`Self::name`]
    /// reads, so a fourth warning variant must choose its vocabulary row
    /// there rather than falling through a wildcard.
    ///
    /// ```rust
    /// # use relate_model::wire::PlanWarning;
    /// # use relate_model::wire::NotificationKind;
    /// assert_eq!(
    ///     PlanWarning::CartesianProduct.notification_kind(),
    ///     NotificationKind::CartesianProduct
    /// );
    /// assert!(PlanWarning::EstimationMiss
    ///     .notification_kind()
    ///     .is_plan_warning());
    /// ```
    pub fn notification_kind(&self) -> NotificationKind {
        self.identity().1
    }

    /// The warning's message text — the vocabulary's base text
    /// ([`Self::base_text`]'s one spelling), with the payload appended in
    /// parentheses when the warning carries one (the `LabelDisjunctionScan`
    /// warning's suggested index pairs). The one spelling the engine
    /// renderer and every surface's table layout render, so the text
    /// cannot drift between the forms.
    pub fn message(&self) -> String {
        match self.suggested_index() {
            Some(index) => format!("{} ({index})", self.base_text()),
            None => self.base_text(),
        }
    }

    /// The wire parse — the one validating match back into the typed
    /// vocabulary: a name from it with the payload shape its variant
    /// carries (`LabelDisjunctionScan` requires the pairs, the payload-less
    /// variants carry none). `None` names a warning outside the
    /// vocabulary or a payload shape its variant does not carry — a
    /// wire-contract break the accessor fails loudly on, never a
    /// silently dropped or reinterpreted warning. In-crate today (the
    /// accessor and the model's tests); re-widened to `pub` in the same
    /// change as the first cross-crate consumer parsing raw wire
    /// warning maps.
    pub(crate) fn from_wire_parts(name: &str, index: Option<&str>) -> Option<Self> {
        match index {
            None => PAYLOAD_LESS.into_iter().find(|w| w.name() == name),
            Some(indexes) => {
                // The payload variant's name comes from `identity()`
                // like every other, not a second hand comparison.
                let candidate = PlanWarning::LabelDisjunctionScan {
                    indexes: indexes.to_string(),
                };
                (candidate.name() == name).then_some(candidate)
            }
        }
    }
}

/// Whether the estimated-vs-actual ratio is defined (ADR-0344 metric
/// fields): the operator produced rows. The one predicate both the
/// ratio cell and the overshoot policy read — never a second
/// `actual > 0` spelling at either site.
pub fn ratio_defined(actual: u64) -> bool {
    actual > 0
}

/// The one estimated-vs-actual overshoot policy (ADR-0344 metric
/// fields + typed node warnings): the estimate misses when it
/// overshoots the actual rows by 10x or more and the operator
/// produced rows (an estimate over zero actual rows has no ratio).
/// Both the ratio cell's warning marker and the `EstimationMiss`
/// warning derive from this one policy, never a second computation.
pub fn estimation_miss(estimated: u64, actual: u64) -> bool {
    // Integer comparison — exact for every saturated u64 (f64's 53-bit
    // mantissa would lose the boundary); floor division preserves the
    // "ten times or more" test for actual > 0.
    ratio_defined(actual) && estimated / ESTIMATION_MISS_FACTOR >= actual
}

/// The wire shape of one typed warning: `{name, index?}`, built from
/// the one [`PlanWarning`] vocabulary — the spelling the plan-wire
/// warning projections read (the HTTP v2 plan), never a re-assembled
/// `{name, index}` at each site. The agent-native document does not read
/// it: per ADR-0381 its node `warnings` derive from the result's
/// remedy-bearing notifications.
pub fn warning_wire_json(warning: &PlanWarning) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert(
        plan_keys::WARNING_NAME.to_string(),
        serde_json::Value::String(warning.name().to_string()),
    );
    if let Some(index) = warning.suggested_index() {
        out.insert(
            plan_keys::WARNING_INDEX.to_string(),
            serde_json::Value::String(index.to_string()),
        );
    }
    serde_json::Value::Object(out)
}
