//! The loadtest's incremental fill: one autocommit batch at a time per
//! shape, the same batch shapes the pin statements build, grown until the
//! caller stops at the memory limit (LABS-277 keeps the mid-degree ladder
//! here). Split from the shape types and pin generators so neither module
//! crosses the thousand-line ceiling.

use super::{
    index_statements, middegree_group_link, node_props, GraphShape, IndexCounts, BATCH,
    MID_DEGREE_GROUP, MID_DEGREE_GROUP_HOPS,
};

/// Large-scale fanout hubs. The loadtest grows leaves under these hubs
/// until the memory limit, rather than picking a leaf count up front.
const FILL_HUBS: u32 = 16;
/// Large-scale dense-ring out-degree. Each window is created and linked in
/// one statement, so a rejected batch cannot leave nodes without edges.
const FILL_DEGREE: u32 = 20;

/// One autocommit batch of a filled shape, for the RSS loadtest.
///
/// Index DDL comes first. Later calls keep emitting data batches; the caller
/// stops when the database reports the memory limit. [`ack`][FillCursor::ack]
/// counts what committed. [`nack`][FillCursor::nack] drops the batch that
/// was rejected. `base` yields nothing.
#[derive(Clone)]
pub(crate) struct FillCursor {
    shape: GraphShape,
    attributes: u32,
    indexes: IndexCounts,
    index_stmts: Vec<String>,
    index_at: usize,
    pos: Pos,
    pending: Option<Pending>,
    pub(crate) nodes: u64,
    pub(crate) rels: u64,
}

#[derive(Clone)]
struct Pending {
    pos: Pos,
    index_at: usize,
    kind: TickKind,
}

#[derive(Clone, Copy)]
enum TickKind {
    Index,
    Nodes(u32),
    Rels(u32),
    NodesAndRels { nodes: u32, rels: u32 },
}

#[derive(Clone)]
struct Pos {
    next: u32,
    ring: RingStep,
    hubs_done: bool,
    hub: u32,
    leaf_at: [u32; FILL_HUBS as usize],
}

impl Default for Pos {
    fn default() -> Self {
        Self {
            next: 0,
            ring: RingStep::Create,
            hubs_done: false,
            hub: 0,
            leaf_at: [0; FILL_HUBS as usize],
        }
    }
}

#[derive(Clone)]
enum RingStep {
    Create,
    Link { start: u32, last: u32 },
    Bridge { start: u32, last: u32 },
}

impl FillCursor {
    pub(crate) fn new(shape: GraphShape, attributes: u32, indexes: IndexCounts) -> Self {
        Self {
            shape,
            attributes,
            indexes,
            index_stmts: index_statements(shape, indexes),
            index_at: 0,
            pos: Pos::default(),
            pending: None,
            nodes: 0,
            rels: 0,
        }
    }

    /// The next statement, or `None` when the shape cannot grow (`base`).
    pub(crate) fn next_statement(&mut self) -> Result<Option<String>, String> {
        if self.pending.is_some() {
            return Err("fill cursor has an unacked statement".into());
        }
        if self.index_at < self.index_stmts.len() {
            let statement = self.index_stmts[self.index_at].clone();
            self.pending = Some(Pending {
                pos: self.pos.clone(),
                index_at: self.index_at,
                kind: TickKind::Index,
            });
            self.index_at += 1;
            return Ok(Some(statement));
        }
        if self.shape == GraphShape::Base {
            return Ok(None);
        }
        let saved = self.pos.clone();
        let (statement, kind) = self.emit_data()?;
        self.pending = Some(Pending {
            pos: saved,
            index_at: self.index_at,
            kind,
        });
        Ok(Some(statement))
    }

    pub(crate) fn ack(&mut self) -> Result<(), String> {
        let pending = self
            .pending
            .take()
            .ok_or("fill cursor has nothing to ack")?;
        match pending.kind {
            TickKind::Index => {}
            TickKind::Nodes(n) => self.nodes += u64::from(n),
            TickKind::Rels(n) => self.rels += u64::from(n),
            TickKind::NodesAndRels { nodes, rels } => {
                self.nodes += u64::from(nodes);
                self.rels += u64::from(rels);
            }
        }
        Ok(())
    }

    pub(crate) fn nack(&mut self) -> Result<(), String> {
        let pending = self
            .pending
            .take()
            .ok_or("fill cursor has nothing to roll back")?;
        self.pos = pending.pos;
        self.index_at = pending.index_at;
        Ok(())
    }

    /// Close a ring whose committed batches are each fully linked.
    ///
    /// Absent when the rejected statement was a link or a bridge: that batch
    /// of nodes is already committed and is not part of the cycle.
    pub(crate) fn ring_wrap_statement(&self) -> Option<String> {
        if self.shape != GraphShape::Ring || self.pending.is_some() {
            return None;
        }
        if !matches!(self.pos.ring, RingStep::Create) || self.nodes < 2 {
            return None;
        }
        let last = self.nodes - 1;
        Some(format!(
            "MATCH (a:Ring {{id: {last}}}), (b:Ring {{id: 0}}) CREATE (a)-[:NEXT]->(b)"
        ))
    }

    pub(crate) fn note_wrap(&mut self) {
        self.rels += 1;
    }

    fn emit_data(&mut self) -> Result<(String, TickKind), String> {
        let attributes = self.attributes;
        let indexes = self.indexes;
        let mut pos = self.pos.clone();
        let out = match self.shape {
            GraphShape::Flat => emit_flat(&mut pos, attributes, indexes)?,
            GraphShape::Ring => emit_ring(&mut pos, attributes, indexes)?,
            GraphShape::Fanout => emit_fanout(&mut pos, attributes, indexes)?,
            GraphShape::DenseRing => emit_dense(&mut pos, attributes, indexes)?,
            GraphShape::MidDegree => emit_middegree(&mut pos, attributes, indexes)?,
            GraphShape::Base => unreachable!("base has no data statements"),
        };
        self.pos = pos;
        Ok(out)
    }
}

fn id_window(start: u32, width: u32) -> Result<(u32, u32), String> {
    let last = start
        .checked_add(width - 1)
        .ok_or("node id overflow while filling")?;
    Ok((start, last))
}

fn advance(last: u32) -> Result<u32, String> {
    last.checked_add(1)
        .ok_or_else(|| "node id overflow while filling".into())
}

fn emit_flat(
    pos: &mut Pos,
    attributes: u32,
    indexes: IndexCounts,
) -> Result<(String, TickKind), String> {
    let (start, last) = id_window(pos.next, BATCH)?;
    let props = node_props("i", attributes, indexes);
    let statement = format!("UNWIND range({start}, {last}) AS i CREATE (:Flat {{{props}}})");
    pos.next = advance(last)?;
    Ok((statement, TickKind::Nodes(BATCH)))
}

fn emit_ring(
    pos: &mut Pos,
    attributes: u32,
    indexes: IndexCounts,
) -> Result<(String, TickKind), String> {
    match pos.ring {
        RingStep::Create => {
            let (start, last) = id_window(pos.next, BATCH)?;
            let props = node_props("i", attributes, indexes);
            let statement =
                format!("UNWIND range({start}, {last}) AS i CREATE (:Ring {{{props}}})");
            pos.ring = RingStep::Link { start, last };
            Ok((statement, TickKind::Nodes(last - start + 1)))
        }
        RingStep::Link { start, last } => {
            let statement = format!(
                "MATCH (c:Ring) WHERE c.id >= {start} AND c.id <= {last} \
                 WITH c ORDER BY c.id \
                 WITH collect(c) AS nodes \
                 UNWIND range(0, size(nodes) - 2) AS i \
                 WITH nodes[i] AS a, nodes[i + 1] AS b \
                 CREATE (a)-[:NEXT]->(b)"
            );
            let links = last - start;
            if start > 0 {
                pos.ring = RingStep::Bridge { start, last };
            } else {
                pos.next = advance(last)?;
                pos.ring = RingStep::Create;
            }
            Ok((statement, TickKind::Rels(links)))
        }
        RingStep::Bridge { start, last } => {
            let prev = start - 1;
            let statement = format!(
                "MATCH (a:Ring {{id: {prev}}}), (b:Ring {{id: {start}}}) \
                 CREATE (a)-[:NEXT]->(b)"
            );
            pos.next = advance(last)?;
            pos.ring = RingStep::Create;
            Ok((statement, TickKind::Rels(1)))
        }
    }
}

fn emit_fanout(
    pos: &mut Pos,
    attributes: u32,
    indexes: IndexCounts,
) -> Result<(String, TickKind), String> {
    if !pos.hubs_done {
        let last = FILL_HUBS - 1;
        let props = node_props("h", attributes, indexes);
        let statement = format!("UNWIND range(0, {last}) AS h CREATE (:Hub {{{props}}})");
        pos.hubs_done = true;
        return Ok((statement, TickKind::Nodes(FILL_HUBS)));
    }
    let hub = pos.hub;
    let start = pos.leaf_at[hub as usize];
    let (_, last) = id_window(start, BATCH)?;
    let props = node_props("l", attributes, indexes);
    let statement = format!(
        "MATCH (hub:Hub {{id: {hub}}}) \
         UNWIND range({start}, {last}) AS l \
         CREATE (hub)-[:LEAF]->(:Leaf {{{props}}})"
    );
    pos.leaf_at[hub as usize] = advance(last)?;
    pos.hub = (hub + 1) % FILL_HUBS;
    Ok((
        statement,
        TickKind::NodesAndRels {
            nodes: BATCH,
            rels: BATCH,
        },
    ))
}

fn emit_dense(
    pos: &mut Pos,
    attributes: u32,
    indexes: IndexCounts,
) -> Result<(String, TickKind), String> {
    // One statement creates the window and links it. A capacity failure rolls
    // both back, so every committed node has its `FILL_DEGREE` edges.
    let width = (BATCH / FILL_DEGREE).max(1);
    let (start, last) = id_window(pos.next, width)?;
    let total = advance(last)?;
    let nodes = u64::from(total);
    let props = node_props("i", attributes, indexes);
    let statement = format!(
        "UNWIND range({start}, {last}) AS i \
         CREATE (:DenseRing {{{props}}}) \
         WITH count(*) AS created \
         UNWIND range({start}, {last}) AS i \
         UNWIND range(1, {FILL_DEGREE}) AS d \
         WITH i, (i + d) % {nodes} AS j \
         MATCH (a:DenseRing {{id: i}}), (b:DenseRing {{id: j}}) \
         CREATE (a)-[:LINK]->(b)"
    );
    pos.next = total;
    Ok((
        statement,
        TickKind::NodesAndRels {
            nodes: width,
            rels: width * FILL_DEGREE,
        },
    ))
}

fn emit_middegree(
    pos: &mut Pos,
    attributes: u32,
    indexes: IndexCounts,
) -> Result<(String, TickKind), String> {
    // One ladder group per statement, created and linked together (the
    // dense fill's shape: a rejected batch leaves no committed nodes
    // without their edges). The modulus total is the nodes committed so
    // far, so a group's forward references stay inside what exists.
    let (start, last) = id_window(pos.next, MID_DEGREE_GROUP)?;
    let total = advance(last)?;
    let props = node_props("i", attributes, indexes);
    let statement = format!(
        "UNWIND range({start}, {last}) AS i CREATE (:MidDegree {{{props}}}) \
         WITH count(*) AS created {}",
        middegree_group_link(start, total)
    );
    // One whole group: the cursor only ever advances by `MID_DEGREE_GROUP`,
    // so the group's hop count is the ladder's own sum.
    debug_assert_eq!(total - start, MID_DEGREE_GROUP);
    let rels = MID_DEGREE_GROUP_HOPS;
    pos.next = total;
    Ok((
        statement,
        TickKind::NodesAndRels {
            nodes: last - start + 1,
            rels,
        },
    ))
}
