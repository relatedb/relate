//! The fixed namespaces of a Relate store (ADR-0116 §1).
//!
//! Every store has exactly three namespaces: [`Namespace::Neo4j`] (the user
//! data graph), [`Namespace::System`] (CE system database; holds no graph
//! data) and [`Namespace::SystemOntologies`] (the read-only ontology graph).
//! None can be created, dropped or renamed, and names match
//! case-insensitively. A namespace whose visibility tracks a preview
//! feature (today [`Namespace::SystemOntologies`], ADR-0593) is invisible
//! while that feature is disabled: listed by no catalog, resolved by no
//! name — see [`namespace_preview_feature`].

use std::fmt;
use std::str::FromStr;

use crate::preview::{PreviewFeature, PreviewFeatures};
use crate::{ce_messages, Error};

/// One of the three fixed namespaces of a store.
///
/// Element ids name their namespace through the middle segment of
/// `<kind>:<segment>:<n|r><id>` (ADR-0116 §7, ADR-0554): the segment is
/// dash-bearing and names the database — `rl-id` for `neo4j`,
/// `rl-onto` for `system_ontologies` — and `system` has
/// no elements and no
/// segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Namespace {
    /// The user data graph. The only namespace Cypher can write.
    Neo4j,
    /// The CE system database: SHOW commands, supported administration
    /// commands and procedure calls only. Holds no graph data.
    System,
    /// The read-only ontology graph, written only through the `db.ontology.*`
    /// procedures. Its dialect visibility gates behind the `ontology`
    /// preview feature (ADR-0593): invisible — unlisted, unaddressable —
    /// while the opt-in is off.
    SystemOntologies,
}

impl Namespace {
    /// Every namespace, in canonical order: `neo4j`, `system`,
    /// `system_ontologies`.
    pub const ALL: [Namespace; 3] = [
        Namespace::Neo4j,
        Namespace::System,
        Namespace::SystemOntologies,
    ];

    /// The canonical (lowercase) namespace name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Namespace::Neo4j => "neo4j",
            Namespace::System => "system",
            Namespace::SystemOntologies => "system_ontologies",
        }
    }

    /// Parses a namespace name, case-insensitively (ADR-0116 §1). Returns
    /// `None` for any other name.
    pub fn parse(name: &str) -> Option<Namespace> {
        Namespace::ALL
            .iter()
            .find(|ns| name.eq_ignore_ascii_case(ns.as_str()))
            .copied()
    }

    /// Whether Cypher can write this namespace. Only `neo4j` is writable
    /// (ADR-0116 §5: read many, write one).
    pub fn is_writable(&self) -> bool {
        matches!(self, Namespace::Neo4j)
    }

    /// Whether this namespace is read-only to Cypher. Only
    /// `system_ontologies` is (ADR-0116 §1).
    pub fn is_read_only(&self) -> bool {
        matches!(self, Namespace::SystemOntologies)
    }

    /// Whether this namespace is visible under `preview` (ADR-0593): a
    /// namespace gated by [`namespace_preview_feature`] stays invisible
    /// until its feature's opt-in is set; the ungated namespaces are
    /// always visible. The one predicate every visibility consumer
    /// derives from — the `SHOW DATABASES` listing, `USE` and connect
    /// resolution, `ALTER DATABASE` targets.
    pub fn visible_under(self, preview: PreviewFeatures) -> bool {
        namespace_preview_feature(self).is_none_or(|feature| preview.enabled(feature))
    }

    /// The visibility gate (ADR-0593): `Ok(())` when
    /// [`Self::visible_under`] holds, otherwise the ordinary
    /// unknown-database rejection (42N00, [`ce_messages::graph_not_found`])
    /// — the name-gate form ADR-0371 prescribes for hidden names: never
    /// the authored preview error, which would advertise a name the
    /// catalog must not list.
    pub fn require_visible(self, preview: PreviewFeatures) -> Result<(), Error> {
        if self.visible_under(preview) {
            Ok(())
        } else {
            Err(ce_messages::graph_not_found(self.as_str()))
        }
    }

    /// The element-id middle segment for this namespace (ADR-0116 §7,
    /// ADR-0554), or `None` for `system`, which has no elements. The
    /// segment is dash-bearing and names the database, as Neo4j's UUID
    /// segment does — clients classify an id as an element id by that
    /// observable dash (ADR-0554's Context).
    pub fn element_id_segment(&self) -> Option<&'static str> {
        match self {
            Namespace::Neo4j => Some("rl-id"),
            Namespace::System => None,
            Namespace::SystemOntologies => Some("rl-onto"),
        }
    }

    /// The inverse of [`Namespace::element_id_segment`]: maps an element-id
    /// middle segment back to its namespace. Returns `None` for any other
    /// segment (including `system`, which has none).
    pub fn from_element_id_segment(segment: &str) -> Option<Namespace> {
        Namespace::ALL
            .iter()
            .find(|ns| ns.element_id_segment() == Some(segment))
            .copied()
    }
}

/// The one fixed-namespace → preview-feature mapping (ADR-0593): a
/// namespace whose dialect visibility tracks a preview feature's opt-in.
/// Today exactly one row — `system_ontologies` behind the `ontology` row
/// (ADR-0378's feature; its contract tracks a non-final external spec,
/// and the namespace exists to host that feature's stored document) —
/// and the shape is the registry pattern, the namespace twin of the
/// executor's procedure-to-feature mapping: a future gated namespace
/// adds its row here, and every consumer — [`Namespace::visible_under`],
/// [`Namespace::require_visible`] and the resolution sites that consult
/// them — follows without new machinery.
fn namespace_preview_feature(namespace: Namespace) -> Option<PreviewFeature> {
    match namespace {
        Namespace::SystemOntologies => Some(PreviewFeature::Ontology),
        Namespace::Neo4j | Namespace::System => None,
    }
}

/// The composite-database `graph.*` selector function names, lowercased
/// (ADR-0116 §3). One shared list: the planner's `use_target` resolves
/// these calls as `USE` selectors (with element-id resolution for
/// `graph.byElementId`) and rejects them with CE's 42N72 text in
/// expression position, while the executor's `SHOW FUNCTIONS` catalog
/// lists them as catalog-only entries (discoverable but not executable).
/// Two separate lists would drift.
pub const GRAPH_SELECTOR_FUNCTIONS: [(&str, usize); 4] = [
    ("graph.names", 0),
    ("graph.byname", 1),
    ("graph.byelementid", 1),
    ("graph.propertiesbyname", 1),
];

/// The selector *names*, derived from [`GRAPH_SELECTOR_FUNCTIONS`] — the one
/// table — for consumers that need a flat `&[&str]` (the executor's
/// catalog-only `SHOW FUNCTIONS` entries).
pub const GRAPH_SELECTOR_FUNCTION_NAMES: [&str; GRAPH_SELECTOR_FUNCTIONS.len()] = {
    let mut names = [""; GRAPH_SELECTOR_FUNCTIONS.len()];
    let mut i = 0;
    while i < GRAPH_SELECTOR_FUNCTIONS.len() {
        names[i] = GRAPH_SELECTOR_FUNCTIONS[i].0;
        i += 1;
    }
    names
};

/// True for the composite-database `graph.*` selector names
/// (case-insensitive). The one predicate home — callers never spell the
/// membership test themselves. Allocation-free: compares with
/// `eq_ignore_ascii_case` like `Namespace::parse` does.
pub fn is_graph_selector_function_name(name: &str) -> bool {
    graph_selector_arity(name).is_some()
}

/// The one arity table for the `graph.*` selector calls: `graph.names()`
/// takes no argument, the three element-name lookups take one. Every site
/// that resolves or rejects these calls checks arity through this table
/// first, so a wrong argument count reports Neo4j's 42I13 arity failure
/// rather than the composite-database 42N72 refusal (SemanticErrorPart3
/// "graph.byElementId with no argument is rejected"). `None` for a name
/// that is not a selector.
pub fn graph_selector_arity(name: &str) -> Option<usize> {
    GRAPH_SELECTOR_FUNCTIONS
        .iter()
        .find(|(selector, _)| name.eq_ignore_ascii_case(selector))
        .map(|(_, arity)| *arity)
}

/// The parsed shape of an element id (ADR-0116 §7):
/// `<kind>:<segment>:<n|r><id>`. This is the one parser for that three-part
/// structure, shared by `USE graph.byElementId(...)` resolution and
/// `db.nameFromElementId`; it only splits the shape — callers decide how
/// much of it to validate (`db.nameFromElementId` checks the kind prefix
/// and rejects an empty entity segment, but deliberately does not validate
/// the entity segment's contents).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementIdRef<'a> {
    /// The first segment: the element kind (`4` node, `5` relationship).
    pub kind: &'a str,
    /// The middle segment: names the namespace, dash-bearing
    /// (ADR-0554): `rl-id` or `rl-onto`.
    pub segment: &'a str,
    /// The third segment: the entity reference (`n<id>` / `r<id>`), opaque.
    pub id: &'a str,
}

impl<'a> ElementIdRef<'a> {
    /// Split an element id into its three colon-separated segments, or
    /// `None` when it does not have exactly that shape.
    pub fn parse(id: &'a str) -> Option<ElementIdRef<'a>> {
        let mut parts = id.split(':');
        let parsed = ElementIdRef {
            kind: parts.next()?,
            segment: parts.next()?,
            id: parts.next()?,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(parsed)
    }

    /// Whether the first segment names an element kind the engine writes —
    /// the one place the kind digit is resolved against
    /// [`ElementKind`]'s vocabulary, so a caller never re-spells it.
    pub fn has_known_kind(&self) -> bool {
        [ElementKind::Node, ElementKind::Rel]
            .into_iter()
            .any(|kind| kind.kind_segment() == self.kind)
    }

    /// The namespace the middle segment names (ADR-0116 §7), or `None`
    /// when it names no namespace.
    pub fn namespace(&self) -> Option<Namespace> {
        Namespace::from_element_id_segment(self.segment)
    }
}

/// The element kind an element id encodes: node (`4`/`n`) or relationship
/// (`5`/`r`). The digit and the entity prefix travel together, so a
/// mismatched pair cannot be expressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementKind {
    Node,
    Rel,
}

impl ElementKind {
    /// The first segment of the wire form (`4` node, `5` relationship).
    fn kind_segment(self) -> &'static str {
        match self {
            ElementKind::Node => "4",
            ElementKind::Rel => "5",
        }
    }

    /// The entity prefix of the third segment (`n`/`r`).
    fn entity_prefix(self) -> char {
        match self {
            ElementKind::Node => 'n',
            ElementKind::Rel => 'r',
        }
    }
}

/// The element id string of a node in `namespace` (`4:<segment>:n<id>`),
/// built through the same validated namespace the parser accepts — an
/// ad-hoc segment could emit an id the engine's own parser refuses.
pub fn node_element_id(id: i64, namespace: Namespace) -> Option<String> {
    element_id(id, namespace, ElementKind::Node)
}

/// The element id string of a relationship in `namespace`
/// (`5:<segment>:r<id>`).
pub fn rel_element_id(id: i64, namespace: Namespace) -> Option<String> {
    element_id(id, namespace, ElementKind::Rel)
}

/// The node id encoded in a relate element id, or `None` for a
/// relationship id, a foreign namespace, or a malformed value.
pub fn parse_node_element_id(s: &str) -> Option<i64> {
    parse_element_id(s, ElementKind::Node)
}

/// The relationship id encoded in a relate element id, or `None`.
pub fn parse_rel_element_id(s: &str) -> Option<i64> {
    parse_element_id(s, ElementKind::Rel)
}

/// One owner of the element-id wire form: the constructor and the decoder
/// share the kind digit, the namespace (validated through the canonical
/// [`Namespace::element_id_segment`] mapping — a bare `starts_with` would
/// accept `relateXYZ`), and the entity prefix.
fn element_id(id: i64, namespace: Namespace, kind: ElementKind) -> Option<String> {
    let segment = namespace.element_id_segment()?;
    Some(format!(
        "{}:{}:{}{}",
        kind.kind_segment(),
        segment,
        kind.entity_prefix(),
        id
    ))
}

fn parse_element_id(s: &str, kind: ElementKind) -> Option<i64> {
    let parsed = ElementIdRef::parse(s)?;
    if parsed.kind != kind.kind_segment() {
        return None;
    }
    parsed.namespace()?;
    parsed.id.strip_prefix(kind.entity_prefix())?.parse().ok()
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Namespace {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Namespace::parse(s).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(Namespace::parse("neo4j"), Some(Namespace::Neo4j));
        assert_eq!(Namespace::parse("Neo4j"), Some(Namespace::Neo4j));
        assert_eq!(Namespace::parse("NEO4J"), Some(Namespace::Neo4j));
        assert_eq!(Namespace::parse("SYSTEM"), Some(Namespace::System));
        assert_eq!(
            Namespace::parse("System_Ontologies"),
            Some(Namespace::SystemOntologies)
        );
        assert_eq!("neo4j".parse::<Namespace>(), Ok(Namespace::Neo4j));
        assert_eq!(
            "SYSTEM_ONTOLOGIES".parse::<Namespace>(),
            Ok(Namespace::SystemOntologies)
        );
    }

    #[test]
    fn parse_rejects_unknown_names() {
        assert_eq!(Namespace::parse(""), None);
        assert_eq!(Namespace::parse("nosuch"), None);
        assert_eq!(Namespace::parse("systemx"), None);
        assert_eq!(Namespace::parse("system_ontology"), None);
        assert!("movies".parse::<Namespace>().is_err());
    }

    #[test]
    fn display_and_as_str_are_canonical() {
        assert_eq!(Namespace::Neo4j.as_str(), "neo4j");
        assert_eq!(Namespace::System.as_str(), "system");
        assert_eq!(Namespace::SystemOntologies.as_str(), "system_ontologies");
        assert_eq!(Namespace::Neo4j.to_string(), "neo4j");
        assert_eq!(Namespace::System.to_string(), "system");
        assert_eq!(Namespace::SystemOntologies.to_string(), "system_ontologies");
    }

    #[test]
    fn only_neo4j_is_writable() {
        assert!(Namespace::Neo4j.is_writable());
        assert!(!Namespace::System.is_writable());
        assert!(!Namespace::SystemOntologies.is_writable());
    }

    #[test]
    fn only_system_ontologies_is_read_only() {
        assert!(!Namespace::Neo4j.is_read_only());
        assert!(!Namespace::System.is_read_only());
        assert!(Namespace::SystemOntologies.is_read_only());
    }

    #[test]
    fn system_ontologies_is_the_only_preview_gated_namespace() {
        // The pinned mapping, mirroring the executor's procedure-gate
        // test: the gated set is derived over the full vocabulary, so a
        // namespace added to the enum fails this pin until its gating is
        // decided here, and one moved in or out of the mapping fails the
        // equality.
        assert_eq!(
            namespace_preview_feature(Namespace::SystemOntologies),
            Some(PreviewFeature::Ontology)
        );
        let gated: Vec<Namespace> = Namespace::ALL
            .iter()
            .copied()
            .filter(|ns| namespace_preview_feature(*ns).is_some())
            .collect();
        assert_eq!(gated, [Namespace::SystemOntologies]);
    }

    #[test]
    fn visibility_follows_the_mapping_and_hides_with_the_ordinary_rejection() {
        let off = PreviewFeatures::none();
        let on = PreviewFeatures::parse("ontology").unwrap();
        assert!(!Namespace::SystemOntologies.visible_under(off));
        assert!(Namespace::SystemOntologies.visible_under(on));
        // The ungated set is the mapping's complement over the full
        // vocabulary, never a hand-spelled subset: a namespace added to
        // the enum is pinned always-visible until its gating is decided.
        for ungated in Namespace::ALL
            .iter()
            .copied()
            .filter(|ns| namespace_preview_feature(*ns).is_none())
        {
            assert!(ungated.visible_under(off), "{ungated}");
            assert!(ungated.visible_under(on), "{ungated}");
        }
        // The gate's disabled form is the ordinary unknown-database
        // rejection — never the authored preview error, which would
        // advertise the hidden name (ADR-0371's name-gate rule).
        let err = Namespace::SystemOntologies
            .require_visible(off)
            .unwrap_err();
        assert!(
            matches!(&err, Error::DatabaseNotFound { name } if name == "system_ontologies"),
            "{err}"
        );
        assert!(Namespace::SystemOntologies.require_visible(on).is_ok());
    }

    #[test]
    fn element_id_segments_round_trip() {
        assert_eq!(Namespace::Neo4j.element_id_segment(), Some("rl-id"));
        assert_eq!(Namespace::System.element_id_segment(), None);
        assert_eq!(
            Namespace::SystemOntologies.element_id_segment(),
            Some("rl-onto")
        );
        for ns in Namespace::ALL {
            if let Some(segment) = ns.element_id_segment() {
                assert_eq!(Namespace::from_element_id_segment(segment), Some(ns));
            }
        }
        // `system` has no segment, and unknown segments map to nothing.
        assert_eq!(Namespace::from_element_id_segment("system"), None);
        assert_eq!(Namespace::from_element_id_segment("rl-id.system"), None);
        assert_eq!(Namespace::from_element_id_segment("neo4j"), None);
        assert_eq!(Namespace::from_element_id_segment(""), None);
    }

    #[test]
    fn element_ids_carry_a_dash_so_shape_sniffing_clients_classify_them() {
        // ADR-0554: real Neo4j ids carry dashes (the database UUID
        // segment), and clients — Neo4j Browser's neighbour expansion is
        // the recorded case — pick the element-id code path by exactly
        // that observable property, parsing the string as a legacy
        // integer otherwise. Every emitted id must keep a dash.
        for ns in Namespace::ALL {
            if ns.element_id_segment().is_some() {
                assert!(node_element_id(7, ns).unwrap().contains('-'));
                assert!(rel_element_id(9, ns).unwrap().contains('-'));
            }
        }
    }

    #[test]
    fn element_id_ref_parses_the_three_part_shape() {
        let parsed = ElementIdRef::parse("4:rl-onto:r9").unwrap();
        assert_eq!(parsed.kind, "4");
        assert_eq!(parsed.segment, "rl-onto");
        assert_eq!(parsed.id, "r9");
        assert_eq!(parsed.namespace(), Some(Namespace::SystemOntologies));
        assert_eq!(
            ElementIdRef::parse("5:rl-id:n1").unwrap().namespace(),
            Some(Namespace::Neo4j)
        );
        // Only the shape is split: kind and entity segment stay opaque.
        let parsed = ElementIdRef::parse("4:rl-id:999999").unwrap();
        assert_eq!(parsed.id, "999999");
        // Not exactly three segments, or a segment naming no namespace.
        assert_eq!(ElementIdRef::parse("garbage"), None);
        assert_eq!(ElementIdRef::parse("4:rl-id"), None);
        assert_eq!(ElementIdRef::parse("4:rl-id:n1:extra"), None);
        assert_eq!(ElementIdRef::parse("4::n1").unwrap().namespace(), None);
        assert_eq!(
            ElementIdRef::parse("4:00000000-0000-0000-0000-000000000000:n1")
                .unwrap()
                .namespace(),
            None
        );
    }

    #[test]
    fn graph_selector_functions_are_the_four_composite_selectors() {
        assert_eq!(
            GRAPH_SELECTOR_FUNCTIONS,
            [
                ("graph.names", 0),
                ("graph.byname", 1),
                ("graph.byelementid", 1),
                ("graph.propertiesbyname", 1)
            ]
        );
        assert_eq!(
            GRAPH_SELECTOR_FUNCTION_NAMES,
            [
                "graph.names",
                "graph.byname",
                "graph.byelementid",
                "graph.propertiesbyname"
            ]
        );
        // Already lowercase: `is_graph_selector_function_name` matches
        // case-insensitively against them (`eq_ignore_ascii_case`).
        for (name, _) in GRAPH_SELECTOR_FUNCTIONS {
            assert_eq!(name, name.to_ascii_lowercase());
        }
        assert!(is_graph_selector_function_name("Graph.ByName"));
        assert!(!is_graph_selector_function_name("graph.x"));
    }

    #[test]
    fn all_lists_every_namespace_in_order() {
        assert_eq!(
            Namespace::ALL,
            [
                Namespace::Neo4j,
                Namespace::System,
                Namespace::SystemOntologies
            ]
        );
    }
}
