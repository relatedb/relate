//! The fulltext corpus subsystem: one index's tokenized documents and
//! per-field statistics, their retained-byte estimate, and the
//! bounded-recency memo that caches them — the scan substrate
//! [`super`]'s orchestration reads, re-exported through
//! `exec::fulltext` like the analyzer and query modules.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::Arc;

use relate_core::{PropertyMap, Value};

use super::Analyzer;
use crate::exec::candidates::index_population;
use crate::exec::kernels::memo::MemoPolicy;
use crate::metrics::MemoCounters;
use crate::overlay::{ExecGraph, MemoLive};
use crate::recency::RecencySlots;
use crate::redo::IndexDetailSpec;

/// One indexed property's tokens for one document. The property travels
/// with its tokens, so a clause's `property:` filter reads fields directly
/// instead of indexing back into `detail.properties`.
pub(super) struct DocField {
    pub(super) property: Arc<str>,
    pub(super) tokens: Vec<String>,
}

impl DocField {
    /// The field's retained allocation: the token slots' capacity beside
    /// each token's text.
    fn heap_bytes(&self) -> usize {
        let mut bytes = self.tokens.capacity() * std::mem::size_of::<String>();
        for token in &self.tokens {
            bytes += token.len();
        }
        bytes
    }
}

/// Per-field corpus statistics: how many indexed entities carry the field,
/// the field's total token count (for `avgdl`), and the document frequency
/// of each term.
#[derive(Default)]
pub(super) struct FieldStats {
    pub(super) docs: usize,
    pub(super) total_len: usize,
    pub(super) df: HashMap<String, usize>,
}

impl FieldStats {
    /// The statistics' retained allocation: the `df` map's slot array
    /// beside each term's text.
    fn heap_bytes(&self) -> usize {
        let mut bytes =
            self.df.capacity() * (std::mem::size_of::<String>() + std::mem::size_of::<usize>());
        for term in self.df.keys() {
            bytes += term.len();
        }
        bytes
    }
}

/// The tokens a value contributes to a fulltext field: a string through
/// the index's analyzer, a list element-wise, recursively (Neo4j fulltext
/// indexes string arrays), anything else nothing. The recursion is
/// unreachable for stored data — a nested list cannot be a property value
/// at all (live 2026.10.1 rejects it with 22N39, "cannot be stored in
/// properties") — but it keeps the walker total over the value model
/// instead of silently dropping an inner list.
fn value_tokens(value: &Value, index: Analyzer) -> Vec<String> {
    match value {
        Value::String(text) => index.analyze(text),
        Value::List(values) => values
            .iter()
            .flat_map(|value| value_tokens(value, index))
            .collect(),
        _ => Vec::new(),
    }
}

fn doc_fields(properties: &[Arc<str>], values: &PropertyMap, index: Analyzer) -> Vec<DocField> {
    // A field is emitted only when it carries tokens: the property's
    // value is present AND tokenizes to at least one term. An absent
    // property, a non-string value, or a string the analyzer strips bare
    // all yield no tokens to match — a `DocField` for them would only be
    // filtered back out by every reader downstream, and the BM25
    // statistics pass counts documents per carried field, so a zero-term
    // field must not reach it either (Lucene's own document count counts
    // only documents with terms in the field).
    properties
        .iter()
        .filter_map(|property| {
            let tokens = values
                .get(property)
                .map(|value| value_tokens(value, index))
                .unwrap_or_default();
            (!tokens.is_empty()).then(|| DocField {
                property: Arc::clone(property),
                tokens,
            })
        })
        .collect()
}

/// The corpus statistics pass over the scan's documents: how many
/// documents carry each indexed property, each field's total token count
/// (for `avgdl`), and every carried term's document frequency. Every
/// indexed property gets an entry from the property list itself — a
/// property no document carries keeps its zero entry — so the score read
/// is an invariant, not a lookup that can miss.
fn field_stats(docs: &[CorpusDoc], properties: &[Arc<str>]) -> HashMap<Arc<str>, FieldStats> {
    let mut stats: HashMap<Arc<str>, FieldStats> = properties
        .iter()
        .map(|property| (Arc::clone(property), FieldStats::default()))
        .collect();
    for doc in docs {
        for field in &doc.fields {
            let stat = stats
                .get_mut(&*field.property)
                .expect("statistics exist for every indexed property");
            stat.docs += 1;
            stat.total_len += field.tokens.len();
            let mut seen = HashSet::new();
            for token in &field.tokens {
                if seen.insert(token.as_str()) {
                    *stat.df.entry(token.clone()).or_insert(0) += 1;
                }
            }
        }
    }
    stats
}

/// One indexed document: its id, its entity's own properties (the same
/// `Arc` the read snapshot holds — no copy, no extra fetch, so the SEARCH
/// inner-WHERE gate filters a matching document without re-reading the
/// record or re-deriving its entity kind, ADR-0678), and its tokenized
/// fields.
pub(super) struct CorpusDoc {
    pub(super) id: i64,
    pub(super) properties: Arc<PropertyMap>,
    pub(super) fields: Vec<DocField>,
}

impl CorpusDoc {
    /// The document's retained allocation: the field slots' capacity
    /// beside each field's own total. The property map is Arc-shared
    /// with the store and deliberately not counted: its bytes are the
    /// store's, not the memo's.
    fn heap_bytes(&self) -> usize {
        let mut bytes = self.fields.capacity() * std::mem::size_of::<DocField>();
        for field in &self.fields {
            bytes += field.heap_bytes();
        }
        bytes
    }
}

/// One index's tokenized documents and per-field statistics: a pure
/// function of the graph and the index's detail and analyzer, so it is
/// built once and read by every scan over the same index.
pub(super) struct Corpus {
    pub(super) docs: Vec<CorpusDoc>,
    pub(super) stats: HashMap<Arc<str>, FieldStats>,
}

impl Corpus {
    /// The corpus's retained allocation — each level of the layout
    /// estimates its own beside its declaration, and this sum adds only
    /// the two facts the corpus alone owns: the documents' slot array,
    /// and the statistics map's slots with each indexed property name's
    /// `Arc<str>` payload once (the fields and the statistics share the
    /// arcs). Every retained text is in the estimate and every container
    /// charges its capacity, so the memo's charge and refund bound the
    /// set the memo actually keeps alive.
    fn heap_bytes(&self) -> usize {
        let mut bytes = self.docs.capacity() * std::mem::size_of::<CorpusDoc>();
        for doc in &self.docs {
            bytes += doc.heap_bytes();
        }
        bytes += self.stats.capacity()
            * (std::mem::size_of::<Arc<str>>() + std::mem::size_of::<FieldStats>());
        for (property, stat) in &self.stats {
            bytes += property.len() + stat.heap_bytes();
        }
        bytes
    }

    fn build(graph: &ExecGraph<'_>, detail: &IndexDetailSpec, index: Analyzer) -> Self {
        let properties: Vec<Arc<str>> = detail.properties.iter().map(|p| Arc::from(&**p)).collect();
        // The shared index population (exec::candidates); the document
        // build and the corpus statistics are this scan's own.
        let docs: Vec<CorpusDoc> = index_population(graph, detail)
            .iter()
            .map(|(id, values)| CorpusDoc {
                id: *id,
                properties: Arc::clone(values),
                fields: doc_fields(&properties, values, index),
            })
            .collect();
        let stats = field_stats(&docs, &properties);
        Self { docs, stats }
    }
}

/// How many distinct `(index, analyzer)` corpora one memo keeps alive —
/// the constant bound on retained *slot count* regardless of who owns the
/// memo. A SEARCH clause's source list is statement text (a COMBINE
/// alternating its sources hits every slot); the `db.index.fulltext.query*`
/// path evaluates `indexName` per input row, so an `UNWIND` over many
/// index names would otherwise retain one fully-tokenized corpus per name
/// touched — memory scaling with runtime data, never with the statement.
/// The slot count and the charged byte total bound the memo together
/// ([`CorpusMemo`]): whichever the next insert would cross first stops
/// the growth.
const MEMO_CAPACITY: usize = 16;

/// One retained corpus beside the bytes its insert charged — the refund
/// on eviction is exactly what the charge added, so the running total
/// cannot drift from the retained set.
struct CorpusSlot {
    detail: IndexDetailSpec,
    analyzer: Analyzer,
    corpus: Rc<Corpus>,
    charged: u64,
}

/// The corpora a SEARCH clause or `db.index.fulltext.query*` call scans,
/// keyed by `(index detail, index analyzer)`. Those scans run once per
/// input row (and per growth round), yet each index's corpus is the same
/// every time: the memo builds each on its first scan and hands every
/// following scan of that index the shared copy.
///
/// Retained memory is bounded twice over, for both callers (a clause's
/// sources are statement text; a procedure call's index names are
/// runtime data): at most [`MEMO_CAPACITY`] slots, and — the bound that
/// holds in *bytes* — the charged total of every retained corpus stays
/// under the kernel memos' own ceiling (`MemoPolicy::moderate`). Every
/// charge is the kernel memo's own three-part protocol
/// (`Memo::remember`): through the statement's retained-container meter
/// (ADR-0463), into the query's shared live-bytes counter (ADR-0657, so
/// the joint per-statement bound sees the corpora — an evicted slot
/// releases its share at once, and the memo's drop returns the rest)
/// and into PROFILE's memo group, the retained bytes folded at the
/// memo's drop (`MemoState::drop`'s own fold, ADR-0654). A refusal on
/// either meter is not an error to the query: the memo is an
/// optimisation, so it freezes — the entry stays, the overshoot is at
/// most this one corpus, and no further insert is attempted. The memo
/// lives as long as the one clause or call that owns it, over a graph
/// view that cannot change beneath it, so nothing invalidates it.
pub(crate) struct CorpusMemo {
    state: RefCell<CorpusMemoState>,
}

/// The memo's live state: the recency-ordered slots, the one retained
/// total, and the freeze flag a refused charge sets. The retained total
/// is the meter's running number and the share charged to the query's
/// live counter at once: every path moves both by the same net delta,
/// and `charge_retained` adds before it checks, so the number is always
/// the retained set's sum.
struct CorpusMemoState {
    slots: RecencySlots<CorpusSlot>,
    charged: u64,
    /// The query's shared live counter, captured at construction — the
    /// graph's own `memo_live`, the same counter every kernel memo of
    /// the statement charges, so the corpora join the joint
    /// per-statement bound from the first insert.
    live: MemoLive,
    frozen: bool,
}

impl Drop for CorpusMemoState {
    /// The memo dies with its clause or call: the retained bytes fold
    /// into PROFILE's memo group when an arm is armed (the kernel
    /// memo's own drop fold, `MemoState::drop`, ADR-0654 — otherwise
    /// nothing is read), then every corpus still retained returns its
    /// share to the query's shared counter (`Memo::release_reserved`).
    fn drop(&mut self) {
        crate::exec::record_memos(|| MemoCounters {
            bytes: self.charged,
            ..MemoCounters::default()
        });
        self.live.release(self.charged);
    }
}

impl CorpusMemo {
    /// An empty memo — the shared recency table at the slot-count cap,
    /// charging the graph's live counter from the first insert.
    pub(crate) fn new(graph: &ExecGraph<'_>) -> Self {
        Self {
            state: RefCell::new(CorpusMemoState {
                slots: RecencySlots::new(
                    NonZeroUsize::new(MEMO_CAPACITY).expect("a non-zero memo cap"),
                ),
                charged: 0,
                live: graph.memo_live(),
                frozen: false,
            }),
        }
    }

    pub(super) fn corpus(
        &self,
        graph: &ExecGraph<'_>,
        detail: &IndexDetailSpec,
        index: Analyzer,
    ) -> Rc<Corpus> {
        let mut state = self.state.borrow_mut();
        // The one recency policy (the shared `RecencySlots`): a hit
        // refreshes its slot to the back.
        if let Some(slot) = state
            .slots
            .hit(|slot| slot.detail == *detail && slot.analyzer == index)
        {
            return Rc::clone(&slot.corpus);
        }
        // The kernel memo's own freeze gate
        // (`exec::kernels::memo::Memo::remember`): the scan's corpus is
        // built either way (the query needs it), but only a kept slot
        // pays the estimate and the settle — a frozen or at-ceiling
        // memo's misses cost a build and nothing more, so the corpus
        // rides back uncached and unmeasured; a kept slot settles both
        // meters by the one comparison — the new corpus against the
        // evicted refund.
        let corpus = Rc::new(Corpus::build(graph, detail, index));
        let ceiling = MemoPolicy::moderate(&state.live).max_bytes as u64;
        if state.frozen || state.charged >= ceiling {
            return corpus;
        }
        let bytes = corpus.heap_bytes() as u64;
        let refund = state
            .slots
            .insert(CorpusSlot {
                detail: detail.clone(),
                analyzer: index,
                corpus: Rc::clone(&corpus),
                charged: bytes,
            })
            .map_or(0, |evicted| evicted.charged);
        // Both meters move by the same delta, so the one retained total
        // serves both: growth charges each (a refusal on either freezes
        // — the kernel memo charges the shared counter even after a
        // meter refusal, and both add before they check), a shrink
        // releases each, an equal swap touches neither.
        match bytes.cmp(&refund) {
            std::cmp::Ordering::Greater => {
                let grew = bytes - refund;
                if graph.charge_retained(&mut state.charged, grew).is_err() {
                    state.frozen = true;
                }
                if state.live.charge(grew) {
                    state.frozen = true;
                }
            }
            std::cmp::Ordering::Less => {
                let shrank = refund - bytes;
                state.charged -= shrank;
                state.live.release(shrank);
            }
            std::cmp::Ordering::Equal => {}
        }
        corpus
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One named source's index detail, the tests' own shape.
    fn detail(property: &str) -> IndexDetailSpec {
        IndexDetailSpec {
            kind: crate::redo::IndexKindSpec::Fulltext,
            relationship: false,
            labels: vec!["Doc".to_string()],
            properties: vec![property.to_string()],
            with_properties: Vec::new(),
        }
    }

    #[test]
    fn the_memo_keeps_one_corpus_per_named_source() {
        let g = crate::graph::Graph::default();
        let graph = ExecGraph::read(&g);
        let memo = CorpusMemo::new(&graph);
        let first = memo.corpus(&graph, &detail("a"), Analyzer::Standard);
        // The same index and analyzer shares the first build.
        assert!(Rc::ptr_eq(
            &first,
            &memo.corpus(&graph, &detail("a"), Analyzer::Standard)
        ));
        // A different analyzer tokenizes differently and a different index
        // holds different documents: each is its own corpus, kept alive
        // for the statement's lifetime — a COMBINE alternating sources
        // hits both slots instead of thrashing one.
        let folding = memo.corpus(&graph, &detail("a"), Analyzer::StandardFolding);
        assert!(!Rc::ptr_eq(&first, &folding));
        let other = memo.corpus(&graph, &detail("b"), Analyzer::Standard);
        assert!(!Rc::ptr_eq(&folding, &other));
        assert!(Rc::ptr_eq(
            &other,
            &memo.corpus(&graph, &detail("b"), Analyzer::Standard)
        ));
        // Every scan of the statement's sources still answers the shared
        // copy — re-scanning the first index after the others hits its
        // slot (the old one-slot memo would have rebuilt it).
        assert!(Rc::ptr_eq(
            &first,
            &memo.corpus(&graph, &detail("a"), Analyzer::Standard)
        ));
        // The capacity bound and the recency policy: fill the table to
        // the cap, re-use the first-built corpus (its slot refreshes to
        // the back), then overflow — the eviction victim is the
        // least-recently-USED slot (a-folding, untouched since built),
        // not the oldest-built, so retained memory is the cap AND the
        // still-in-use corpora stay resident.
        for slot in 0..MEMO_CAPACITY - live_slots(&memo) {
            memo.corpus(&graph, &detail(&format!("n{slot}")), Analyzer::Standard);
        }
        assert_eq!(live_slots(&memo), MEMO_CAPACITY);
        // The re-use: a-std was touched last of the original three, so
        // its slot is the freshest — the overflow evicts a-folding.
        let refreshed = memo.corpus(&graph, &detail("a"), Analyzer::Standard);
        assert!(Rc::ptr_eq(&first, &refreshed));
        memo.corpus(&graph, &detail("one-too-many"), Analyzer::Standard);
        assert_eq!(live_slots(&memo), MEMO_CAPACITY);
        // a-std survives the overflow (it was just used); the untouched
        // a-folding is gone — its next scan rebuilds.
        assert!(Rc::ptr_eq(
            &first,
            &memo.corpus(&graph, &detail("a"), Analyzer::Standard)
        ));
        assert!(!Rc::ptr_eq(
            &folding,
            &memo.corpus(&graph, &detail("a"), Analyzer::StandardFolding)
        ));
        // The byte bound: the charged total equals the sum over exactly
        // the retained slots' own estimates — the overflow's eviction
        // refunded the evicted slot's charge, so the total never sums
        // everything ever built.
        let sum_of_retained = {
            let state = memo.state.borrow();
            state.slots.iter().map(|slot| slot.charged).sum::<u64>()
        };
        assert_eq!(charged_bytes(&memo), sum_of_retained);
        assert!(sum_of_retained > 0);
        // One more overflow: the new charge lands and the evicted refund
        // holds — the total stays the retained set's sum.
        memo.corpus(&graph, &detail("n-overflow"), Analyzer::Standard);
        let state = memo.state.borrow();
        let sum_after = state.slots.iter().map(|slot| slot.charged).sum::<u64>();
        assert_eq!(state.charged, sum_after);
        assert_eq!(state.slots.len(), MEMO_CAPACITY);
        // The shared live counter (ADR-0657): every retained corpus
        // charges there too, an eviction releases at once, so the
        // query-wide figure is the same retained sum — never everything
        // ever built, and visible to the joint bound the kernel memos
        // charge against.
        drop(state);
        assert_eq!(graph.memo_live().live_bytes(), sum_after);
    }

    /// The retained estimate covers every retained text — the tokens',
    /// the property name's payload, the `df` keys' — not just the
    /// containers' slots: the same corpus with empty strings estimates
    /// exactly the texts' bytes less.
    #[test]
    fn the_retained_estimate_counts_every_retained_text() {
        let corpus = |property: &str, token: &str| Corpus {
            docs: vec![CorpusDoc {
                id: 1,
                properties: Arc::default(),
                fields: vec![DocField {
                    property: Arc::from(property),
                    tokens: vec![token.to_string()],
                }],
            }],
            stats: HashMap::from([(
                Arc::from(property),
                FieldStats {
                    docs: 1,
                    total_len: 1,
                    df: HashMap::from([(token.to_string(), 1)]),
                },
            )]),
        };
        let texts = "lampa".len() * 2 + "name".len();
        assert_eq!(
            corpus("name", "lampa").heap_bytes() - corpus("", "").heap_bytes(),
            texts
        );
    }

    /// The live slot count, read through the private state for the
    /// capacity pins.
    fn live_slots(memo: &CorpusMemo) -> usize {
        memo.state.borrow().slots.len()
    }

    /// The running charged-bytes total, read through the private state
    /// for the byte-bound pins.
    fn charged_bytes(memo: &CorpusMemo) -> u64 {
        memo.state.borrow().charged
    }

    /// The memo's retained bytes fold into PROFILE's memo group at the
    /// memo's drop (the kernel memo's own drop fold, ADR-0654): the
    /// thread's group is a never-reset accumulator, so the pin reads it
    /// as a delta — the snapshot pattern every canonical reader uses —
    /// and the arm restores what it was (a pre-armed test thread stays
    /// armed, LABS-805).
    #[test]
    fn the_retained_bytes_fold_into_the_profile_memo_group() {
        let g = crate::graph::Graph::default();
        let graph = ExecGraph::read(&g);
        let prior = crate::exec::arm_store_profiling();
        let before = crate::exec::memo_counters();
        let memo = CorpusMemo::new(&graph);
        memo.corpus(&graph, &detail("a"), Analyzer::Standard);
        memo.corpus(&graph, &detail("b"), Analyzer::Standard);
        let expected = charged_bytes(&memo);
        drop(memo);
        let after = crate::exec::memo_counters();
        crate::exec::restore_store_profiling(prior);
        assert!(expected > 0);
        assert_eq!(after.since(before).bytes, expected);
    }
}
