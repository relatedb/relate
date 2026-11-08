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

    /// The LIVE version: the newest one with `xmax == 0`.
    pub(super) fn live(&self) -> Option<VerRef<'_, Live>> {
        self.iter().rev().find(|v| v.xmax == 0)
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
        if let Self::Inline { xmin, .. } = self {
            let xmin = *xmin;
            let Self::Inline { live, .. } = std::mem::replace(self, Self::Spilled(Vec::new()))
            else {
                unreachable!()
            };
            *self = Self::Spilled(vec![Ver {
                xmin,
                xmax: ts,
                live,
            }]);
        } else if let Self::Spilled(vers) = self {
            let v = vers.iter_mut().rev().find(|v| v.xmax == 0)?;
            v.xmax = ts;
        }
        match self {
            Self::Spilled(vers) => vers.iter().rev().find(|v| v.xmax == ts).map(|v| &v.live),
            Self::Inline { .. } => unreachable!("the inline row was spilled above"),
        }
    }

    /// A single live version goes back inline.
    fn fold(&mut self) {
        if let Self::Spilled(vers) = self {
            if vers.len() == 1 && vers[0].xmax == 0 {
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
