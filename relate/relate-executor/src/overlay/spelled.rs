//! The statement's user-spelled internal-namespace names and the `__`
//! namespace's own policies (REL8-1732).
//!
//! The executor parks its own synthetic row columns (`__ord*` sort keys,
//! `__agg_*` aggregate seeds, `__a*` anonymous elements, `__node*` /
//! `__trail*` pattern seats) under a `__` prefix, and every star-shaped
//! consumer hides those. A user may spell the same prefix on a real
//! binding (`WITH i AS `__x``): that column is declared by the statement,
//! so it is not internal. The typed fact is the statement's declared
//! scope, recorded once at statement entry through the canonical plan walk
//! (nested bodies included, so the set is complete before any clause runs
//! and no execution order can change it) and restarted at every statement
//! entry beside the other per-statement state; a row key is internal
//! exactly when it is in the `__` namespace and the statement never
//! declared it. The synthetic row-column prefixes (`__ord`, `__a`,
//! `__agg_c`, and the `__node` / `__trail` seats) are chosen through
//! [`SpelledNames::uncolliding_prefix`], so none of those columns can ever
//! carry a declared name: a name declared only inside a nested body is
//! declared statement-wide, and no executor-made column elsewhere can be
//! mistaken for it. The one fixed spelling is the anonymous shortest-path
//! binding (`__shortest_path`, exec/shortest_eval.rs), which stays
//! hidden as an internal key unless the statement itself spells it.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use crate::pattern_walk::SeatPrefixes;

/// Whether `name` is in the reserved `__` namespace the executor's
/// synthetic columns live in. Only a *candidate* test: a user may spell
/// the prefix on a real binding, so a row key is internal — never
/// projected to users — exactly when [`ExecGraph::is_internal_key`](crate::overlay::ExecGraph::is_internal_key)
/// says so, which also asks whether the statement declared the name. The
/// declaration walk itself ([`SpelledNames::declare`], which decides what
/// to record) and the `__` namespace reservation
/// (`projection_collides_with_internal_names`, a static decline over the
/// plan's aliases that has no row to ask about) read the raw prefix.
pub(crate) fn is_internal_column(name: &str) -> bool {
    name.starts_with("__")
}

/// `base` lengthened with `_` until no name in `names` starts with it —
/// the raw one-pass half of the freshening policy, private to this module:
/// the declared-aware two-pass form ([`SpelledNames::uncolliding_prefix`],
/// reached crate-wide through `ExecGraph::uncolliding_prefix`) is the only
/// public path, so no caller can lengthen past the caller's names while
/// skipping the declared set. One pass over the names, linear in their
/// total length (a name can be as long as the query cap): the prefix needs
/// one more `_` than the longest run of them any name carries right after
/// `base`.
fn uncolliding_prefix<'n>(base: &str, names: impl IntoIterator<Item = &'n str>) -> String {
    let longest_run = names
        .into_iter()
        .filter_map(|name| name.strip_prefix(base))
        .map(|rest| rest.bytes().take_while(|&byte| byte == b'_').count())
        .max();
    let mut prefix = String::from(base);
    if let Some(run) = longest_run {
        prefix.extend(std::iter::repeat_n('_', run + 1));
    }
    prefix
}

#[derive(Default)]
pub(crate) struct SpelledNames {
    declared: RefCell<HashSet<String>>,
    /// The statement's seat-prefix pair, resolved on first use and cached:
    /// the declared set is fixed after statement entry, so the freshened
    /// prefixes cannot change mid-statement.
    seats: RefCell<Option<Arc<SeatPrefixes>>>,
}

impl SpelledNames {
    pub(crate) fn reset(&self) {
        self.declared.borrow_mut().clear();
        *self.seats.borrow_mut() = None;
    }

    /// The statement's anonymous-seat prefixes ([`SeatPrefixes`]),
    /// resolved once through the declared-aware freshening policy and
    /// cached for the statement — the data the pattern-walk schema
    /// builders take instead of the live view.
    pub(crate) fn seat_prefixes(&self) -> Arc<SeatPrefixes> {
        if let Some(seats) = self.seats.borrow().as_ref() {
            return Arc::clone(seats);
        }
        let seats = Arc::new(SeatPrefixes {
            node: self.uncolliding_prefix("__node", std::iter::empty()),
            trail: self.uncolliding_prefix("__trail", std::iter::empty()),
        });
        *self.seats.borrow_mut() = Some(Arc::clone(&seats));
        seats
    }

    /// Record one declared name; only the `__`-prefixed ones matter.
    pub(crate) fn declare(&self, name: &str) {
        if !is_internal_column(name) {
            return;
        }
        // The cached seat pair was freshened against the declared set, so
        // a late declaration drops it: the pair is then resolved again on
        // the next use, never left half-applied.
        *self.seats.borrow_mut() = None;
        self.declared.borrow_mut().insert(name.to_string());
    }

    /// [`uncolliding_prefix`] over `names` plus every declared name: the
    /// one freshening policy for a synthetic `__` column prefix of this
    /// statement.
    pub(crate) fn uncolliding_prefix<'n>(
        &self,
        base: &str,
        names: impl IntoIterator<Item = &'n str>,
    ) -> String {
        // Two passes: lengthening past the caller's names, then past the
        // declared set from that result — the second prefix extends the
        // first, so it still misses every name the first one missed.
        let past_names = uncolliding_prefix(base, names);
        let declared = self.declared.borrow();
        uncolliding_prefix(&past_names, declared.iter().map(String::as_str))
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.declared.borrow().contains(name)
    }
}
