//! The per-element compiler of the ontology → graph-type conversion
//! (ADR-0226 §2): [`Element`] — the node or relationship entry that
//! owns the properties and constraints being compiled — plus the
//! per-token classification its declarations read ([`TokenMapping`],
//! [`Neo4jType::mapping`]) and the constraint correspondence its
//! definitions compile through ([`ConstraintType::graph_type_verb`],
//! [`constraint_spec`]).
//!
//! An element carries its id, whether it compiles to an edge type spec
//! (the one encoding of node-vs-relationship), and its properties and
//! constraints in document order. The methods here derive everything
//! one entry contributes to the body — typed property declarations,
//! standalone constraint definitions and skip reports — leaving the
//! document-level orchestration (entry ordering, endpoint resolution,
//! the relationship-type declaration policy of ADR-0668) to
//! [`to_graph_type_body`](super::to_graph_type_body).
//!
//! A property's type classifies once, at the [`Element::declaration`]
//! boundary: [`Neo4jType::mapping`] hands the declared token and the
//! property's vector dimension to one [`TokenMapping`] — the forms with
//! no graph-type equivalent carry their skip report's words, the
//! representable forms their CIP-176 value type — so the representable
//! arm re-asserts nothing.

use std::collections::{HashMap, HashSet};

use relate_names::SmallName;
use relate_parser::{
    GraphTypeConstraintSpec, GraphTypeConstraintVerb, GraphTypeElementRef, GraphTypePropertyType,
    OptionsForm, PredefinedKind, TypeExpr, VectorCoordKind,
};

use crate::model::{
    flagged_constraint, Constraint, ConstraintType, Neo4jType, NodeType, Property, RelationshipType,
};
use crate::OntologyError;

/// What one `Neo4jType` token compiles to in a CIP-176 property type.
///
/// The match producing it is exhaustive over [`Neo4jType`]: a token
/// added to the closed enum must be classified here before the crate
/// compiles, so no token can be silently skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenMapping {
    /// A predefined CIP-176 leaf of the same name.
    Predefined(PredefinedKind),
    /// `LIST<X>`: CIP-176 property types are closed, so the element is
    /// always `NOT NULL`.
    List(PredefinedKind),
    /// `VECTOR<X>`: the coordinate kind with the property's `dimension`
    /// resolved — the one boundary that decides a vector's
    /// representability classifies a dimensionless one unrepresentable,
    /// so a representable vector never re-checks for a dimension.
    Vector {
        coord: VectorCoordKind,
        dimension: i64,
    },
    /// `ANY`, `LIST<ANY>` and a `VECTOR<…>` without a dimension: the
    /// document's claim is "unconstrained", which no graph-type
    /// property type expresses. The carried words name the form in the
    /// skip report; the property is reported as skipped, and a required
    /// one keeps its requirement as `ANY NOT NULL`.
    Unrepresentable(&'static str),
}

/// One `VECTOR<X>` token's classification with the property's
/// `dimension`: the coordinate kind plus the resolved dimension — a
/// dimensionless vector is the unrepresentable form (a graph-type
/// vector property type requires one).
fn vector_mapping(coord: VectorCoordKind, dimension: Option<i64>) -> TokenMapping {
    match dimension {
        Some(dimension) => TokenMapping::Vector { coord, dimension },
        None => TokenMapping::Unrepresentable("dimensionless vector property"),
    }
}

impl Neo4jType {
    /// The ONE token classification, read everywhere the mapping decides
    /// between a declaration and a skipped entry. The property's
    /// `dimension` rides along so the vector forms resolve theirs here.
    fn mapping(self, dimension: Option<i64>) -> TokenMapping {
        use Neo4jType as T;
        match self {
            T::Any => TokenMapping::Unrepresentable("untyped property"),
            T::ListAny => TokenMapping::Unrepresentable("untyped list property"),
            T::Boolean => TokenMapping::Predefined(PredefinedKind::Boolean),
            T::ListBoolean => TokenMapping::List(PredefinedKind::Boolean),
            T::Date => TokenMapping::Predefined(PredefinedKind::Date),
            T::ListDate => TokenMapping::List(PredefinedKind::Date),
            T::Duration => TokenMapping::Predefined(PredefinedKind::Duration),
            T::ListDuration => TokenMapping::List(PredefinedKind::Duration),
            // The narrow numerics widen for scalars and lists (ADR-0667):
            // the property-type constraint still holds for narrower
            // values, so the widening never asserts less than the
            // document.
            T::Float32 => TokenMapping::Predefined(PredefinedKind::Float),
            T::ListFloat32 => TokenMapping::List(PredefinedKind::Float),
            T::Float => TokenMapping::Predefined(PredefinedKind::Float),
            T::ListFloat => TokenMapping::List(PredefinedKind::Float),
            T::Integer8 | T::Integer16 | T::Integer32 => {
                TokenMapping::Predefined(PredefinedKind::Integer)
            }
            T::ListInteger8 | T::ListInteger16 | T::ListInteger32 => {
                TokenMapping::List(PredefinedKind::Integer)
            }
            T::Integer => TokenMapping::Predefined(PredefinedKind::Integer),
            T::ListInteger => TokenMapping::List(PredefinedKind::Integer),
            T::LocalDatetime => TokenMapping::Predefined(PredefinedKind::LocalDateTime),
            T::ListLocalDatetime => TokenMapping::List(PredefinedKind::LocalDateTime),
            T::LocalTime => TokenMapping::Predefined(PredefinedKind::LocalTime),
            T::ListLocalTime => TokenMapping::List(PredefinedKind::LocalTime),
            T::Point => TokenMapping::Predefined(PredefinedKind::Point),
            T::ListPoint => TokenMapping::List(PredefinedKind::Point),
            T::String => TokenMapping::Predefined(PredefinedKind::String),
            T::ListString => TokenMapping::List(PredefinedKind::String),
            T::VectorFloat => vector_mapping(VectorCoordKind::Float64, dimension),
            T::VectorFloat32 => vector_mapping(VectorCoordKind::Float32, dimension),
            T::VectorInteger => vector_mapping(VectorCoordKind::Int64, dimension),
            T::VectorInteger32 => vector_mapping(VectorCoordKind::Int32, dimension),
            T::VectorInteger16 => vector_mapping(VectorCoordKind::Int16, dimension),
            T::VectorInteger8 => vector_mapping(VectorCoordKind::Int8, dimension),
            T::ZonedDatetime => TokenMapping::Predefined(PredefinedKind::ZonedDateTime),
            T::ListZonedDatetime => TokenMapping::List(PredefinedKind::ZonedDateTime),
            T::ZonedTime => TokenMapping::Predefined(PredefinedKind::ZonedTime),
            T::ListZonedTime => TokenMapping::List(PredefinedKind::ZonedTime),
            T::Uuid => TokenMapping::Predefined(PredefinedKind::Uuid),
        }
    }
}

/// The element-level decorations one entry carries — the descriptive
/// members the conversion never compiles and reports as skipped: the
/// element's own aliases, description and reference (a relationship
/// entry's too), plus a node entry's optional labels (a relationship
/// entry declares no labels object). [`From`] fills it from the entry
/// kind, so a decoration added to one kind joins the report by
/// construction.
pub(super) struct ElementDecorations<'a> {
    /// Alternative names for the entry (plural, the element-level
    /// spelling).
    aliases: &'a [String],
    /// Human-readable description.
    description: Option<&'a str>,
    /// Informational URI pointing at an external definition.
    reference: Option<&'a str>,
    /// A node entry's optional labels; a relationship entry declares
    /// none.
    optional_labels: &'a [String],
}

impl<'a> From<&'a NodeType> for ElementDecorations<'a> {
    fn from(entry: &'a NodeType) -> Self {
        Self {
            aliases: &entry.aliases,
            description: entry.description.as_deref(),
            reference: entry.reference.as_deref(),
            optional_labels: &entry.labels.optional,
        }
    }
}

impl<'a> From<&'a RelationshipType> for ElementDecorations<'a> {
    fn from(entry: &'a RelationshipType) -> Self {
        Self {
            aliases: &entry.aliases,
            description: entry.description.as_deref(),
            reference: entry.reference.as_deref(),
            optional_labels: &[],
        }
    }
}

impl ElementDecorations<'_> {
    /// The skip entries one element's decorations contribute, in the
    /// mapper's `"<what> on <element>"` shape: aliases (plural, the
    /// element-level spelling), a node's optional labels, description
    /// and reference — every element-level decoration reported by the
    /// one function.
    fn skips(&self, key: &str, skips: &mut Vec<String>) {
        if !self.aliases.is_empty() {
            skips.push(format!("aliases on {key}"));
        }
        if !self.optional_labels.is_empty() {
            skips.push(format!("optional labels on {key}"));
        }
        for (member, carried) in [
            ("description", self.description),
            ("reference", self.reference),
        ] {
            if carried.is_some_and(|value| !value.is_empty()) {
                skips.push(format!("{member} on {key}"));
            }
        }
    }
}

/// The entry that owns the properties and constraints being compiled, for
/// named [`OntologyError::Mapping`] diagnostics.
///
/// `is_edge` is the one encoding of node-vs-relationship — the word the
/// mapping errors name derives from it. The identifying label its
/// compiled artifacts target is not carried: the target reference
/// every definition aims at is built where the element's
/// body-membership was decided, at the call site.
pub(super) struct Element<'a> {
    /// The entry's id in the document's map.
    pub(super) key: &'a str,
    /// Whether the element compiles to an edge type spec — the one
    /// is-edge fact its constraint definitions carry.
    pub(super) is_edge: bool,
    /// The entry's properties, in document order.
    pub(super) properties: &'a [Property],
    /// The entry's constraint objects, in document order.
    pub(super) constraints: &'a [Constraint],
}

impl<'a> Element<'a> {
    fn mapping(&self, detail: impl std::fmt::Display) -> OntologyError {
        let kind = if self.is_edge { "relationship" } else { "node" };
        OntologyError::Mapping(format!("Ontology {kind} '{}' {detail}", self.key))
    }

    /// The element's property ids mapped to their resolved names — the
    /// display `name` when the entry carries one, the id otherwise (the
    /// one resolution rule, [`Property::resolved_name`]). Two ids
    /// resolving to one name would declare one graph-type property
    /// twice under different addresses, so the first collision fails
    /// loudly.
    fn resolved_names(&self) -> Result<HashMap<&'a str, String>, OntologyError> {
        let mut names = HashMap::with_capacity(self.properties.len());
        let mut seen: HashMap<&str, &str> = HashMap::with_capacity(self.properties.len());
        for property in self.properties {
            let name = property.resolved_name();
            if let Some(_first) = seen.get(name) {
                return Err(self.mapping(format!(
                    "has multiple properties resolving to name '{name}'."
                )));
            }
            seen.insert(name, &property.id);
            names.insert(property.id.as_str(), name.to_owned());
        }
        Ok(names)
    }

    /// The resolved name of one property id, failing loudly when the
    /// element declares no such property — the one resolution every
    /// constraint property reference goes through.
    fn resolve<'n>(
        &self,
        names: &'n HashMap<&'a str, String>,
        id: &str,
    ) -> Result<&'n str, OntologyError> {
        names
            .get(id)
            .map(String::as_str)
            .ok_or_else(|| self.mapping(format!("references unknown property '{id}'.")))
    }

    /// The ids of the element's required properties — every property
    /// flagged `must_exist` plus every property an `EXISTS` constraint
    /// object lists. An `EXISTS` covering several properties has no
    /// graph-type form, and each of its listed ids must resolve.
    fn required_ids(
        &self,
        names: &HashMap<&'a str, String>,
    ) -> Result<HashSet<&'a str>, OntologyError> {
        let mut required: HashSet<&'a str> = self
            .properties
            .iter()
            .filter(|property| property.must_exist == Some(true))
            .map(|property| property.id.as_str())
            .collect();
        for constraint in self.constraints {
            if constraint.constraint_type != ConstraintType::Exists {
                continue;
            }
            if constraint.properties.len() > 1 {
                return Err(self.mapping(
                    "has an existence constraint covering multiple properties, which is not \
                     representable in a graph type.",
                ));
            }
            for id in &constraint.properties {
                self.resolve(names, id)?;
                required.insert(id.as_str());
            }
        }
        Ok(required)
    }

    /// Compile the element — the prelude every element conversion runs,
    /// stated once: the resolution tables, the typed declarations, and
    /// every skip entry the element's properties and decorations
    /// contribute (a relationship entry's endpoint cardinalities stay
    /// at the caller — they are the entry's, not the element's). The
    /// decisions that follow at the call sites — a node's dropped
    /// verdict, the target its constraint definitions aim at — read the
    /// answer.
    pub(super) fn compile(
        &self,
        decorations: ElementDecorations<'_>,
        skips: &mut Vec<String>,
    ) -> Result<CompiledElement<'a>, OntologyError> {
        let names = self.resolved_names()?;
        let required = self.required_ids(&names)?;
        let (declarations, mut property_skips) = self.properties_and_skips(&required)?;
        skips.append(&mut property_skips);
        decorations.skips(self.key, skips);
        Ok(CompiledElement {
            declarations,
            names,
            required,
        })
    }

    /// The element's typed property declarations plus the skips its
    /// properties contribute: the declarations in document order (an
    /// unrepresentable property contributes one only when required, as
    /// `ANY NOT NULL`), and one skip per unrepresentable property and
    /// per carried property decoration.
    fn properties_and_skips(
        &self,
        required: &HashSet<&str>,
    ) -> Result<(Vec<GraphTypePropertyType>, Vec<String>), OntologyError> {
        let mut declarations = Vec::with_capacity(self.properties.len());
        let mut skips = Vec::new();
        for property in self.properties {
            let name = property.resolved_name();
            let (declaration, unrepresentable) = self.declaration(property, name, required)?;
            if let Some(what) = unrepresentable {
                skips.push(format!("{what} {}.{name} (not representable)", self.key));
            }
            if let Some(declaration) = declaration {
                declarations.push(declaration);
            }
            self.property_skips(property, name, &mut skips);
        }
        Ok((declarations, skips))
    }

    /// One property's declaration and skip entry: the declaration is
    /// `None` exactly when the property's type has no graph-type form
    /// and the property is optional; the skip entry's words are `None`
    /// exactly when the property's type has a graph-type form. An empty
    /// resolved name never declares — a graph-type property name is
    /// non-empty — but an optional unrepresentable property still
    /// reports its skip.
    ///
    /// # Errors
    ///
    /// [`OntologyError::Mapping`] for an empty property name that
    /// declares, or a non-positive vector dimension (ADR-0364 §5.4).
    fn declaration(
        &self,
        property: &Property,
        name: &str,
        required: &HashSet<&str>,
    ) -> Result<(Option<GraphTypePropertyType>, Option<&'static str>), OntologyError> {
        let not_null = required.contains(property.id.as_str());
        // No token at all is the untyped form; otherwise the ONE
        // classification — the token's CIP-176 form with a vector's
        // dimension resolved — decides representability here.
        let Some(token) = property.property_type else {
            return self.unrepresentable_declaration(name, not_null, "untyped property");
        };
        match token.mapping(property.dimension) {
            TokenMapping::Unrepresentable(what) => {
                self.unrepresentable_declaration(name, not_null, what)
            }
            TokenMapping::Predefined(kind) => {
                self.typed_declaration(token, name, TypeExpr::Predefined { kind, not_null })
            }
            TokenMapping::List(kind) => self.typed_declaration(
                token,
                name,
                TypeExpr::List {
                    // CIP-176 property types are closed: a list's inner
                    // type is mandatory NOT NULL, so the spec's `LIST<X>`
                    // maps to `LIST<X NOT NULL>` — a synonym mapping,
                    // not a strengthening.
                    element: Box::new(TypeExpr::Predefined {
                        kind,
                        not_null: true,
                    }),
                    not_null,
                },
            ),
            TokenMapping::Vector { coord, dimension } => {
                // The classification resolved the dimension; what is
                // left to fail is a non-positive one.
                if dimension <= 0 {
                    return Err(self.mapping(format!(
                        "property '{name}' has vector dimension {dimension}: a graph-type \
                         vector needs a positive dimension"
                    )));
                }
                self.typed_declaration(
                    token,
                    name,
                    TypeExpr::Vector {
                        coord: Some(coord),
                        dimension: Some(dimension),
                        not_null,
                    },
                )
            }
        }
    }

    /// One property whose classified form has no graph-type equivalent:
    /// the skip entry's words, and — required — the existence alias
    /// `ANY NOT NULL` (an empty resolved name never declares, even
    /// required).
    fn unrepresentable_declaration(
        &self,
        name: &str,
        not_null: bool,
        what: &'static str,
    ) -> Result<(Option<GraphTypePropertyType>, Option<&'static str>), OntologyError> {
        if !not_null {
            return Ok((None, Some(what)));
        }
        if name.is_empty() {
            return Err(self.empty_name("is required"));
        }
        // A required property whose type the body cannot express keeps
        // its requirement as the existence alias.
        Ok((
            Some(GraphTypePropertyType {
                name: SmallName::from_name(name),
                value_type: TypeExpr::Predefined {
                    kind: PredefinedKind::Any,
                    not_null: true,
                },
                default_value: None,
                inline_constraint: None,
            }),
            Some(what),
        ))
    }

    /// One property whose classified form has a graph-type equivalent:
    /// the CIP-176 value type of its classification, `NOT NULL` when
    /// required, with the declared token — read from the property at
    /// the classification boundary — naming it in the empty-name
    /// refusal.
    ///
    /// # Errors
    ///
    /// [`OntologyError::Mapping`] for an empty property name that
    /// declares.
    fn typed_declaration(
        &self,
        token: Neo4jType,
        name: &str,
        value_type: TypeExpr,
    ) -> Result<(Option<GraphTypePropertyType>, Option<&'static str>), OntologyError> {
        if name.is_empty() {
            return Err(self.empty_name(format_args!("declares type '{}'", token.as_str())));
        }
        Ok((
            Some(GraphTypePropertyType {
                name: SmallName::from_name(name),
                value_type,
                default_value: None,
                inline_constraint: None,
            }),
            None,
        ))
    }

    /// The empty-name guard's one message: what the empty-named property
    /// was about to do, and the rule.
    fn empty_name(&self, doing: impl std::fmt::Display) -> OntologyError {
        self.mapping(format!(
            "property '' {doing}: a graph-type property name is non-empty"
        ))
    }

    /// The skip entries one property's decorations contribute, in the
    /// mapper's `"<what> on <element>.<property>"` shape (singular
    /// `alias`, the property-level spelling).
    fn property_skips(&self, property: &Property, name: &str, skips: &mut Vec<String>) {
        let location = format!("{}.{name}", self.key);
        if property
            .one_of
            .as_ref()
            .is_some_and(|one_of| !one_of.is_empty())
        {
            skips.push(format!("one_of on {location}"));
        }
        for (member, carried) in [
            ("pattern", property.pattern.as_deref()),
            ("description", property.description.as_deref()),
            ("reference", property.reference.as_deref()),
        ] {
            if carried.is_some_and(|value| !value.is_empty()) {
                skips.push(format!("{member} on {location}"));
            }
        }
        if !property.aliases.is_empty() {
            skips.push(format!("alias on {location}"));
        }
    }

    /// The element's standalone constraint definitions, in document
    /// order: the `key`/`unique` flag definitions of its properties
    /// first (in property order, named `<name>_<element>_<kind>`), then
    /// its `KEY`/`UNIQUE` constraint objects. `target` is the reference
    /// every definition aims at, built where the element's own
    /// body-membership was decided — a dropped element's constraints
    /// stay, aimed at the label without implying a declared node type.
    pub(super) fn constraint_specs(
        &self,
        names: &HashMap<&'a str, String>,
        required: &HashSet<&str>,
        target: GraphTypeElementRef,
    ) -> Result<Vec<GraphTypeConstraintSpec>, OntologyError> {
        let mut out = Vec::new();
        for property in self.properties {
            let name = property.resolved_name();
            // The flag definitions come from the one naming-policy home
            // (key wins over unique, ADR-0667) — the same home the
            // canonical form's flag-derived entries consult. Bound once
            // above the emptiness check: the guard refuses a flagged
            // empty name off the same derivation the body consumes.
            let flag = flagged_constraint(name, self.key, property.key, property.unique);
            if name.is_empty() {
                if flag.is_some() {
                    return Err(self.empty_name("is asserted by a shorthand flag"));
                }
                if required.contains(property.id.as_str()) {
                    return Err(self.empty_name("is asserted"));
                }
                continue;
            }
            if let Some((kind, generated)) = flag {
                out.push(constraint_spec(
                    Some(generated),
                    self.is_edge,
                    vec![name.to_owned()],
                    // A flag-derived constraint is key or unique — both
                    // carry verbs; exists never reaches here (an
                    // existence requirement compiles into the
                    // declaration's NOT NULL).
                    kind.graph_type_verb()
                        .expect("a flag-derived constraint is key or unique, which carry verbs"),
                    target.clone(),
                ));
            }
        }
        for object in self.constraints {
            if object.properties.is_empty() {
                return Err(self.mapping(format!(
                    "constraint '{}' lists no properties: a graph-type constraint needs at \
                     least one",
                    object.id
                )));
            }
            // An EXISTS object's requirement is already compiled into its
            // properties' declarations; it contributes no definition.
            if object.constraint_type == ConstraintType::Exists {
                continue;
            }
            let mut resolved = Vec::with_capacity(object.properties.len());
            for id in &object.properties {
                if id.is_empty() {
                    return Err(self.mapping(format!(
                        "constraint '{}' lists the empty property name: a graph-type property \
                         name is non-empty",
                        object.id
                    )));
                }
                resolved.push(self.resolve(names, id)?.to_owned());
            }
            let Some(verb) = object.constraint_type.graph_type_verb() else {
                // The kind self-names: today this arm is `PROPERTY_TYPE`
                // alone (`EXISTS` compiled into declarations above), and
                // a future verb-less kind reports its own type, never
                // this arm's current one.
                return Err(self.mapping(format!(
                    "constraint '{}' is of type {}: the graph-type body cannot \
                     express a property-type constraint object",
                    object.id,
                    object.constraint_type.as_str()
                )));
            };
            // A multi-property constraint body names its properties in
            // one canonical order: sorted by resolved name.
            resolved.sort();
            out.push(constraint_spec(
                object.name.clone(),
                self.is_edge,
                resolved,
                verb,
                target.clone(),
            ));
        }
        Ok(out)
    }
}

impl ConstraintType {
    /// The graph-type verb of this constraint kind: `KEY` and `UNIQUE`
    /// carry their verb; `EXISTS` compiles into the property
    /// declaration's `NOT NULL` (or `ANY NOT NULL`), never a standalone
    /// constraint, and `PROPERTY_TYPE` has no graph-type body form —
    /// both answer `None` and the converter fails loudly. The match is
    /// exhaustive over the kind: a new variant fails compilation here
    /// instead of silently answering `None` and misreporting its type
    /// at the read site.
    fn graph_type_verb(self) -> Option<GraphTypeConstraintVerb> {
        match self {
            ConstraintType::Exists => None,
            ConstraintType::Key => Some(GraphTypeConstraintVerb::Key),
            ConstraintType::PropertyType => None,
            ConstraintType::Unique => Some(GraphTypeConstraintVerb::Unique),
        }
    }
}

/// One standalone graph-type constraint, in the target shape the
/// parser's own inline-constraint rewrite produces (`FOR (:Label =>)`):
/// the target reference the caller built where the element's
/// body-membership was decided, `is_edge` marking relationship-owned
/// constraints.
fn constraint_spec(
    name: Option<String>,
    is_edge: bool,
    properties: Vec<String>,
    kind: GraphTypeConstraintVerb,
    target: GraphTypeElementRef,
) -> GraphTypeConstraintSpec {
    GraphTypeConstraintSpec {
        name: name.as_ref().map(|n| SmallName::from_name(n.as_str())),
        target,
        is_edge,
        properties: properties
            .iter()
            .map(|p| SmallName::from_name(p.as_str()))
            .collect(),
        kind,
        property_type: None,
        options: OptionsForm::Absent,
    }
}

/// One element's compiled prelude — what [`Element::compile`] answers:
/// the typed property declarations plus the resolution tables the
/// constraint definitions read.
pub(super) struct CompiledElement<'a> {
    /// The element's typed property declarations, in document order.
    pub(super) declarations: Vec<GraphTypePropertyType>,
    /// The element's property ids mapped to their resolved names.
    pub(super) names: HashMap<&'a str, String>,
    /// The ids of the element's required properties.
    pub(super) required: HashSet<&'a str>,
}
