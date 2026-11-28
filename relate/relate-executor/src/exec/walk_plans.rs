//! The per-pattern walk-plan memo (REL8-841): the two setup products of
//! `match_resolved_path` that depend on the entry row's *names*, never on
//! its values — the compiled scan-predicate specialization
//! ([`WalkSpec`]) and the binding-schema ladder ([`WalkLadder`]) — built
//! once per (pattern, entry schema) and reused by every later driving row
//! of the same clause call.
//!
//! This is a plan cache at the pattern grain, not a result memo: it stores
//! no data and no per-row state, so its memory is a handful of small
//! entries whatever the row count, and the walk's store reads are
//! unchanged.
//!
//! **The invariant that makes the key the schema alone:** the memo lives
//! in the pattern's [`PathFilters`](super::one_path), whose life is one
//! `match_clause` call over one input batch — never across a write and
//! never across statements. Within it the graph, the parameters and the
//! residual's conjunct list are fixed (the residual's `scope` varies per
//! row but is a function of the row's names and is not an input of
//! [`WalkSpec::compile`]), so a spec depends on the entry schema only. A
//! caller that handed one memo two different residuals would break this;
//! every caller builds its [`PathFilters`](super::one_path) beside the
//! one residual it walks under.
//!
//! The entry schema's identity is the interned `Arc<RowSchema>` the rows
//! of one stream share ([`SchemaKey`] holds it, so the address cannot be
//! reused while an entry lives). The start selection's *value* inputs (the
//! bound node id, the estimates) stay per row: only the ladder's arm
//! ([`LadderArm`], the one normalization [`WalkLadder::new`] builds from)
//! keys the ladder. Both tables are bounded recency tables
//! ([`RecencySlots`], the repo's one bounded-memo policy).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::rc::Rc;

use relate_core::Value;
use relate_ir::{Expr, PathPattern};

use crate::overlay::ExecGraph;
use crate::pattern_walk::{LadderArm, Selection, WalkLadder};
use crate::recency::RecencySlots;
use crate::row::{row_bound, Row, SchemaKey};

use super::predicate_spec::WalkSpec;

/// Entries kept per table. A pattern sees one entry schema per input
/// stream and at most a few start arms; a full table evicts its
/// least-recently-used entry.
const CAPACITY: NonZeroUsize = NonZeroUsize::new(8).expect("8 is non-zero");

struct SpecEntry {
    schema: SchemaKey,
    spec: Rc<WalkSpec>,
}

struct LadderEntry {
    schema: SchemaKey,
    arm: LadderArm,
    ladder: Rc<WalkLadder>,
}

/// One pattern's memo of its walk setup (see the module doc).
pub(crate) struct WalkPlans {
    specs: RefCell<RecencySlots<SpecEntry>>,
    ladders: RefCell<RecencySlots<LadderEntry>>,
}

impl Default for WalkPlans {
    fn default() -> Self {
        WalkPlans {
            specs: RefCell::new(RecencySlots::new(CAPACITY)),
            ladders: RefCell::new(RecencySlots::new(CAPACITY)),
        }
    }
}

impl WalkPlans {
    /// The compiled scan-predicate specialization for `path` under `row`'s
    /// entry schema: compiled on the first row of a schema, shared after.
    pub(crate) fn spec(
        &self,
        graph: &ExecGraph<'_>,
        path: &PathPattern,
        params: &BTreeMap<String, Value>,
        conjuncts: &[Expr],
        row: &Row,
    ) -> Rc<WalkSpec> {
        let mut specs = self.specs.borrow_mut();
        if let Some(hit) = specs.hit(|e| e.schema.is(row)) {
            return Rc::clone(&hit.spec);
        }
        // The entry row's bindings are the compile's one row-dependent
        // input, and only through which *names* it owns — the schema.
        let spec = Rc::new(WalkSpec::compile(
            graph,
            path,
            params,
            conjuncts,
            &row_bound(row),
        ));
        specs.insert(SpecEntry {
            schema: row.schema_key(),
            spec: Rc::clone(&spec),
        });
        spec
    }

    /// The binding-schema ladder for the walk `selection` chose, under
    /// `row`'s entry schema: built on the first row of a (schema, arm),
    /// shared after. The ladder reads the entry row's names only.
    pub(crate) fn ladder(
        &self,
        graph: &ExecGraph<'_>,
        path: &PathPattern,
        row: &Row,
        selection: Option<&Selection>,
    ) -> Rc<WalkLadder> {
        let arm = LadderArm::of(selection);
        let mut ladders = self.ladders.borrow_mut();
        if let Some(hit) = ladders.hit(|e| e.schema.is(row) && e.arm == arm) {
            return Rc::clone(&hit.ladder);
        }
        let ladder = Rc::new(WalkLadder::new(
            &graph.seat_prefixes(),
            row,
            path,
            arm,
            path.has_path_name(),
        ));
        ladders.insert(LadderEntry {
            schema: row.schema_key(),
            arm,
            ladder: Rc::clone(&ladder),
        });
        ladder
    }
}
