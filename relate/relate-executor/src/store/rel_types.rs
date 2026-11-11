//! A pattern's static relationship-type disjunction in the interned
//! domain (LABS-801) — the walk engine's type-filter vocabulary, split
//! out of `store.rs` (whose charter is the compact live records, ADR-0033)
//! the way `compact_props` split before it.
//! The pattern carries written names (`RelPattern::types`);
//! [`RelTypes::resolve`] turns them into catalog ids once, at the walk's
//! construction, so the per-edge adjacency walk compares ids and never
//! builds, hashes, or compares a string.

use std::sync::Arc;

use relate_ir::RelPattern;

use super::Interner;

/// The resolved filter: `Any` admits every type (an untyped pattern),
/// `Only` the listed ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RelTypes {
    /// No static filter (an untyped pattern): every type matches.
    Any,
    /// The pattern's type list, deduplicated — the chunk-length sums
    /// (`Graph::typed_adj_len`) and the chunk walk both depend on a type
    /// counting once. Shared behind one `Arc`: the shortest-path
    /// searches clone the filter per derived `Search` (the reversed and
    /// capped variants), and a refcount is what those clones pay.
    Only(Arc<[RelType]>),
}

/// One entry of a resolved [`RelTypes`] filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelType {
    /// The interned catalog id — the key the committed adjacency chunks
    /// and the stored rel versions carry. `None` when the store never
    /// interned the name, so no committed rel has that type (the NFA
    /// compiler's same verdict, `kernels::nfa::Builder::interned`).
    pub(crate) id: Option<u32>,
    /// The written name. The write overlay's rel copies carry type
    /// strings, which an id cannot serve (the `node_has_label_id`
    /// precedent), so the overlay's arm keeps comparing the name. An
    /// interned name's entry holds the interner's own `Arc<str>` unit —
    /// a clone is a refcount, never a copy.
    pub(crate) name: Arc<str>,
}

impl RelTypes {
    /// The one constructor: dedup by name (names are canonical per
    /// interned id — the interner is append-only and never reassigns —
    /// so name and id dedup are interchangeable) and map an empty list
    /// to `Any`. Both public entry points are adapters into it, so the
    /// "a type counts once" invariant and the `Any` coercion live here
    /// alone.
    fn only(entries: impl IntoIterator<Item = (Option<u32>, Arc<str>)>) -> Self {
        let mut only: Vec<RelType> = Vec::new();
        for (id, name) in entries {
            // One canonical identity key: the interned id when there is
            // one, the name otherwise — names are canonical per id (the
            // interner is append-only and never reassigns), so the two
            // keys cannot disagree.
            let duplicate = match id {
                Some(id) => only.iter().any(|t| t.id == Some(id)),
                None => only.iter().any(|t| t.name == name),
            };
            if duplicate {
                continue;
            }
            only.push(RelType { id, name });
        }
        if only.is_empty() {
            return RelTypes::Any;
        }
        RelTypes::Only(only.into())
    }

    /// Resolve `names` — a [`RelPattern::types`] list — against
    /// `interner`. The one place written type names cross into the
    /// interned domain; each walk calls it once at its construction.
    /// An interned name's entry shares the interner's canonical
    /// `Arc<str>`; a never-interned one gets its own copy (nothing owns
    /// it yet).
    ///
    /// [`RelPattern::types`]: relate_ir::RelPattern::types
    pub(crate) fn resolve<T: AsRef<str>>(interner: &Interner, names: &[T]) -> Self {
        Self::only(names.iter().map(|name| {
            let name = name.as_ref();
            Self::entry(interner, name, interner.get_id(name))
        }))
    }

    /// One filter entry for `name` whose interned id is `id` (`None` for a
    /// never-interned name): an interned name shares the interner's
    /// canonical `Arc<str>`, a never-interned one gets its own copy.
    fn entry(interner: &Interner, name: &str, id: Option<u32>) -> (Option<u32>, Arc<str>) {
        (
            id,
            id.map_or_else(|| Arc::from(name), |id| interner.get(id)),
        )
    }

    /// [`Self::resolve`] for a planned pattern: the type ids the planner
    /// resolved onto `rel` (ADR-0541) are read as they are. An unresolved
    /// plan falls back to the interner lookup; a type the plan saw as never
    /// interned has no id.
    pub(crate) fn of_pattern(interner: &Interner, rel: &RelPattern) -> Self {
        Self::only(rel.types.iter().map(|(name, token)| {
            name.with_str(|name| {
                Self::entry(interner, name, token.id_or_else(|| interner.get_id(name)))
            })
        }))
    }

    /// The filter for a type list already in the interned domain — the
    /// frontier NFA's compiled transitions, each id beside the canonical
    /// name the write overlay's arm compares. An empty list is the
    /// untyped filter, exactly like an empty name list at the boundary.
    /// A pattern's `[:R|R]` compiles to two transitions over one id;
    /// the shared constructor's dedup walks the chunk once (the walk
    /// budget pays per visit).
    pub(crate) fn from_interned(types: impl IntoIterator<Item = (u32, Arc<str>)>) -> Self {
        Self::only(types.into_iter().map(|(id, name)| (Some(id), name)))
    }

    /// Whether this is the untyped filter: every relationship type
    /// matches and the walk enumerates every chunk.
    pub(crate) fn is_any(&self) -> bool {
        matches!(self, RelTypes::Any)
    }

    /// The committed-domain ids this filter admits, in pattern order —
    /// the chunk keys the adjacency walk reads. `None` for
    /// [`RelTypes::Any`] (it admits every type — no id list exists), so
    /// a caller cannot read the untyped arm as an empty filter: the two
    /// meanings are distinct values, and the `is_any` pre-gate every
    /// reader used to spell folds into the `None` arm. A name the store
    /// never interned contributes no id.
    pub(crate) fn ids(&self) -> Option<impl Iterator<Item = u32> + Clone + '_> {
        match self {
            RelTypes::Any => None,
            RelTypes::Only(types) => Some(types.iter().filter_map(|t| t.id)),
        }
    }

    /// Whether a rel copy whose written type is `name` matches. The
    /// write overlay holds rel copies keyed by type string, so its arm
    /// compares names; `Any` matches every type, exactly like an untyped
    /// pattern. A committed rel's type name is its interned id's
    /// canonical name (the interner is append-only), so the same
    /// predicate serves the seek paths' in-place filter.
    pub(crate) fn matches_name(&self, name: &str) -> bool {
        match self {
            RelTypes::Any => true,
            RelTypes::Only(types) => types.iter().any(|t| &*t.name == name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_types_resolve_dedups_and_marks_unknown() {
        let i = Interner::default();
        let known = i.intern("KNOWS");
        let types = RelTypes::resolve(&i, &["KNOWS", "KNOWS", "GHOST"]);
        let RelTypes::Only(entries) = &types else {
            panic!("a non-empty name list resolves to Only");
        };
        // The duplicate name resolves once (the chunk sums and the chunk
        // walk both depend on a type counting once), and the name the
        // store never interned keeps its name beside a `None` id.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, Some(known));
        assert_eq!(&*entries[0].name, "KNOWS");
        assert_eq!(entries[1].id, None);
        assert_eq!(&*entries[1].name, "GHOST");
        assert_eq!(types.ids().unwrap().collect::<Vec<_>>(), vec![known]);
        // The name arm serves the write overlay's string-typed rel copies,
        // committed type and never-committed alike.
        assert!(types.matches_name("KNOWS"));
        assert!(types.matches_name("GHOST"));
        assert!(!types.matches_name("OTHER"));
        // An interned name's entry shares the interner's canonical unit.
        assert!(Arc::ptr_eq(&entries[0].name, &i.get(known)));
    }

    #[test]
    fn rel_types_any_and_from_interned() {
        let i = Interner::default();
        assert!(RelTypes::resolve::<String>(&i, &[]).is_any());
        assert!(RelTypes::Any.matches_name("ANYTHING"));
        assert!(RelTypes::Any.ids().is_none());
        let id = i.intern("R");
        let types = RelTypes::from_interned([(id, i.get(id))]);
        assert!(!types.is_any());
        assert_eq!(types.ids().unwrap().collect::<Vec<_>>(), vec![id]);
        assert!(types.matches_name("R"));
        assert!(!types.matches_name("S"));
        assert!(RelTypes::from_interned(Vec::<(u32, Arc<str>)>::new()).is_any());
        // A duplicated id (a `[:R|R]` transition) dedups: the chunk walk
        // and the chunk-length sums count a type once.
        let dup = RelTypes::from_interned([(id, i.get(id)), (id, i.get(id))]);
        assert_eq!(dup.ids().unwrap().collect::<Vec<_>>(), vec![id]);
    }
}
