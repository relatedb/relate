//! The per-id MVCC slot (ADR-0529): an inline live row until the write that
//! creates a second version (or a tombstone) spills it into a version chain;
//! abort and checkpoint GC fold a chain left with one live version back.

use crate::memory::{fit_reserved, ChainShape};

/// One version of a spilled chain.
#[derive(Debug, Clone)]
pub(super) struct Ver<Live> {
    pub(super) xmin: u64,
    pub(super) xmax: u64,
    pub(super) live: Live,
}

impl<Live> Ver<Live> {
    /// The one live rule: a version is live while it is open-ended
    /// (`xmax == 0`).
    pub(super) fn is_live(&self) -> bool {
        self.xmax == 0
    }

    /// A version is collectable once its tombstone is at or before `horizon`.
    fn dead_at(&self, horizon: u64) -> bool {
        self.xmax != 0 && self.xmax <= horizon
    }
}

/// A borrowed version: a spilled [`Ver`], or an inline row read with its
/// birth timestamp as `xmin` and a clear `xmax`.
#[derive(Debug)]
pub(super) struct VerRef<'a, Live> {
    pub(super) xmin: u64,
    pub(super) xmax: u64,
    pub(super) live: &'a Live,
}

impl<Live> Clone for VerRef<'_, Live> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Live> Copy for VerRef<'_, Live> {}

impl<'a, Live> VerRef<'a, Live> {
    fn of(v: &'a Ver<Live>) -> Self {
        Self {
            xmin: v.xmin,
            xmax: v.xmax,
            live: &v.live,
        }
    }

    pub(super) fn dead_at(&self, horizon: u64) -> bool {
        self.xmax != 0 && self.xmax <= horizon
    }
}

/// One id's versions. `Spilled` always holds at least one version, and a
/// single-version `Spilled` always carries a tombstone: a lone live version
/// is folded back to `Inline`.
#[derive(Debug, Clone)]
pub(super) enum Chain<Live> {
    Inline { xmin: u64, live: Live },
    Spilled(Vec<Ver<Live>>),
}

/// A node's inline slot costs the birth timestamp on top of the row: the
/// spilled variant lives in the row's vector-capacity niche. A relationship
/// row has no free niche, so its slot also carries the variant tag. On
/// 32-bit targets the row is not 8-byte aligned and the slot pads.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(
    std::mem::size_of::<Chain<crate::store::LiveNode>>()
        == std::mem::size_of::<u64>() + std::mem::size_of::<crate::store::LiveNode>()
);

/// Oldest-to-newest walk over a [`Chain`].
pub(super) struct ChainIter<'a, Live> {
    inline: Option<VerRef<'a, Live>>,
    spilled: std::slice::Iter<'a, Ver<Live>>,
}

impl<'a, Live> Iterator for ChainIter<'a, Live> {
    type Item = VerRef<'a, Live>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inline
            .take()
            .or_else(|| self.spilled.next().map(VerRef::of))
    }
}

impl<Live> DoubleEndedIterator for ChainIter<'_, Live> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.spilled
            .next_back()
            .map(VerRef::of)
            .or_else(|| self.inline.take())
    }
}

impl<Live> Chain<Live> {
    pub(super) fn inline(xmin: u64, live: Live) -> Self {
        Self::Inline { xmin, live }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Inline { .. } => 1,
            Self::Spilled(vers) => vers.len(),
        }
    }

    pub(super) fn shape(&self) -> ChainShape {
        match self {
            Self::Inline { .. } => ChainShape::Inline,
            Self::Spilled(vers) => ChainShape::Spilled(vers.len()),
        }
    }

    #[cfg(test)]
    pub(super) fn is_inline(&self) -> bool {
        matches!(self, Self::Inline { .. })
    }

    pub(super) fn iter(&self) -> ChainIter<'_, Live> {
        match self {
            Self::Inline { xmin, live } => ChainIter {
                inline: Some(VerRef {
                    xmin: *xmin,
                    xmax: 0,
                    live,
                }),
                spilled: [].iter(),
            },
            Self::Spilled(vers) => ChainIter {
                inline: None,
                spilled: vers.iter(),
            },
        }
    }

    /// The newest version.
    pub(super) fn last(&self) -> VerRef<'_, Live> {
        self.iter()
            .next_back()
            .expect("a version chain is never empty")
    }

    /// The newest version visible at `ts`.
    pub(super) fn visible_at(&self, ts: u64) -> Option<VerRef<'_, Live>> {
        self.iter()
            .rev()
            .find(|v| super::visible(v.xmin, v.xmax, ts))
    }

    /// The LIVE version: the newest one with `xmax == 0` — found from the
    /// end, so the common single-version chain exits on its first item.
    pub(super) fn live(&self) -> Option<VerRef<'_, Live>> {
        self.iter().nth(self.live_index()?)
    }

    /// [`Self::live`]'s position form, for the walks that classify every
    /// version as they pass (a shadowed open-ended version from a
    /// replayed re-create is superseded: only the newest counts). The
    /// reverse scan exits on the newest open-ended hit — the rule
    /// [`Self::live`] states, as an index.
    pub(super) fn live_index(&self) -> Option<usize> {
        match self {
            // The inline version is open-ended by construction.
            Self::Inline { .. } => Some(0),
            Self::Spilled(vers) => vers.iter().rposition(Ver::is_live),
        }
    }

    /// Take the slot's versions as a vector, leaving an empty `Spilled`
    /// placeholder the caller overwrites.
    fn take_versions(&mut self) -> Vec<Ver<Live>> {
        match std::mem::replace(self, Self::Spilled(Vec::new())) {
            Self::Inline { xmin, live } => vec![Ver {
                xmin,
                xmax: 0,
                live,
            }],
            Self::Spilled(vers) => vers,
        }
    }

    /// Append `ver` as the newest version (a replayed create on an id that
    /// already has versions keeps them, as the chain did before).
    pub(super) fn push(&mut self, ver: Ver<Live>) {
        let mut vers = self.take_versions();
        vers.push(ver);
        fit_reserved(&mut vers);
        *self = Self::Spilled(vers);
    }

    /// The newest row, for the write-back after [`Chain::cow`].
    pub(super) fn tail_live_mut(&mut self) -> &mut Live {
        match self {
            Self::Inline { live, .. } => live,
            Self::Spilled(vers) => {
                &mut vers
                    .last_mut()
                    .expect("a version chain is never empty")
                    .live
            }
        }
    }

    /// Tombstone the live version at `ts`, spilling an inline row into a
    /// one-version chain. `None` when nothing is live.
    pub(super) fn tombstone(&mut self, ts: u64) -> Option<&Live> {
        let at = self.live_index()?;
        let mut vers = self.take_versions();
        vers[at].xmax = ts;
        *self = Self::Spilled(vers);
        self.iter().nth(at).map(|v| v.live)
    }

    /// A single live version goes back inline.
    fn fold(&mut self) {
        if let Self::Spilled(vers) = self {
            if vers.len() == 1 && vers[0].is_live() {
                let Ver { xmin, live, .. } = vers.pop().expect("one version");
                *self = Self::Inline { xmin, live };
            }
        }
    }

    /// Undo the write at `ts`: drop the versions it created and clear the
    /// tombstones it set. `false` when no version is left (the write created
    /// the id), so the caller removes the slot.
    pub(super) fn abort(&mut self, ts: u64) -> bool {
        match self {
            Self::Inline { xmin, .. } => *xmin != ts,
            Self::Spilled(vers) => {
                vers.retain(|v| v.xmin != ts);
                for v in vers.iter_mut() {
                    if v.xmax == ts {
                        v.xmax = 0;
                    }
                }
                if vers.is_empty() {
                    return false;
                }
                fit_reserved(vers);
                self.fold();
                true
            }
        }
    }

    pub(super) fn any_dead(&self, horizon: u64) -> bool {
        match self {
            Self::Inline { .. } => false,
            Self::Spilled(vers) => vers.iter().any(|v| v.dead_at(horizon)),
        }
    }

    /// Drop every version dead at `horizon`. `false` when none is left, so
    /// the caller removes the slot.
    pub(super) fn collect(&mut self, horizon: u64) -> bool {
        let Self::Spilled(vers) = self else {
            return true;
        };
        vers.retain(|v| !v.dead_at(horizon));
        if vers.is_empty() {
            return false;
        }
        fit_reserved(vers);
        self.fold();
        true
    }
}

impl<Live: Clone> Chain<Live> {
    /// Copy-on-write: a tombstoned latest version is gone; a version born in
    /// this write is reused; otherwise tombstone the latest version and push
    /// its clone as this write's version. An inline row spills here.
    pub(super) fn cow(&mut self, ts: u64) -> Option<Live> {
        match self {
            Self::Inline { xmin, live } => {
                if *xmin == ts {
                    return Some(live.clone());
                }
                let mut vers = self.take_versions();
                vers[0].xmax = ts;
                let live = vers[0].live.clone();
                vers.push(Ver {
                    xmin: ts,
                    xmax: 0,
                    live: live.clone(),
                });
                fit_reserved(&mut vers);
                *self = Self::Spilled(vers);
                Some(live)
            }
            Self::Spilled(vers) => {
                let last = vers.last_mut()?;
                if last.xmax != 0 {
                    return None;
                }
                if last.xmin == ts {
                    return Some(last.live.clone());
                }
                let live = last.live.clone();
                last.xmax = ts;
                vers.push(Ver {
                    xmin: ts,
                    xmax: 0,
                    live: live.clone(),
                });
                fit_reserved(vers);
                Some(live)
            }
        }
    }
}

#[cfg(test)]
mod tombstone_tests {
    use super::*;

    #[test]
    fn tombstone_spills_an_inline_row_and_returns_it() {
        let mut c = Chain::inline(3, "a");
        assert_eq!(c.tombstone(7), Some(&"a"));
        assert!(!c.is_inline());
        let vers: Vec<_> = c.iter().map(|v| (v.xmin, v.xmax)).collect();
        assert_eq!(vers, [(3, 7)]);
        // Nothing is live any more: a repeat tombstones nothing.
        assert_eq!(c.tombstone(9), None);
    }

    #[test]
    fn tombstone_closes_the_newest_live_version_of_a_spilled_chain() {
        let mut c = Chain::inline(1, "a");
        c.push(Ver {
            xmin: 5,
            xmax: 0,
            live: "b",
        });
        assert_eq!(c.tombstone(8), Some(&"b"));
        let vers: Vec<_> = c.iter().map(|v| (v.xmin, v.xmax)).collect();
        assert_eq!(vers, [(1, 0), (5, 8)]);
    }
}
