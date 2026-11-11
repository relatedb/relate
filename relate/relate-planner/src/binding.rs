//! Variable binding: the scope environment threaded through validation and
//! the rules for introducing pattern variables into it.

use std::collections::HashMap;

use relate_core::{Error, Result};
use relate_ir::{Expr, PathPattern, PatternElement};
use relate_names::SmallName;

use crate::expr_check::literal::{static_literal, LiteralKind};

/// What kind of value a name in scope is bound to. The binder tracks only
/// what later checks need: entity-ness and literal-ness for type-conflict
/// and argument-type errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Binding {
    Node,
    Rel,
    Path,
    Value,
    /// Statically known scalar: a non-null literal (int/bool/string/float)
    /// or a scalar-typed subquery expression (`EXISTS {…}`, `COUNT {…}`).
    /// MATCH as a node/rel is a type conflict.
    Scalar,
    /// Statically known list: a list literal or a `COLLECT {…}` result.
    /// Property access is InvalidArgumentType.
    List,
    /// Literal map. MATCH as a node/rel is a type conflict.
    Map,
}

impl Binding {
    /// Whether the binding holds a non-entity value (a scalar, list or map
    /// literal, or the unconstrained `Value`): exactly the class a
    /// subquery-expression body's pattern may shadow.
    pub(crate) fn is_non_entity(self) -> bool {
        matches!(self, Binding::Scalar | Binding::List | Binding::Map)
    }

    /// The binding an expression's static result type implies, when the
    /// expression is a literal or a typed subquery expression — the one
    /// classifier behind a projection's next-stage scope. A variable's kind
    /// comes from the environment (the caller's arm); an expression whose
    /// type is only known per row returns `None` (the caller binds `Value`).
    ///
    /// The literal kinds are [`static_literal`]'s, so this classifier and
    /// the argument-type rules cannot disagree. `EXISTS {…}` is a BOOLEAN,
    /// `COUNT {…}` an INTEGER, and `COLLECT {…}` a LIST, so reusing a
    /// projected subquery expression as a pattern element is the same type
    /// conflict a literal reuse is (live Neo4j 2026.10.1:
    /// VariableTypeConflict).
    pub(crate) fn of_static_expr(expr: &Expr) -> Option<Binding> {
        match expr {
            Expr::ExistsSubquery { .. } | Expr::CountSubquery { .. } => Some(Binding::Scalar),
            Expr::CollectSubquery { .. } => Some(Binding::List),
            // A folded temporal constructor carries no static binding kind
            // here: the projection's name stays `Value` so component access
            // on it remains legal (`WITH date('…') AS d RETURN d.year`).
            Expr::RuntimeValue(_) => None,
            expr => match static_literal(expr)? {
                LiteralKind::List => Some(Binding::List),
                LiteralKind::Map => Some(Binding::Map),
                LiteralKind::Null => None,
                LiteralKind::Bool | LiteralKind::Int | LiteralKind::Float | LiteralKind::Other => {
                    Some(Binding::Scalar)
                }
            },
        }
    }
}

/// The non-entity names of `env`: the shadowable set a subquery-expression
/// body's pattern may rebind. The one derivation of that classification —
/// [`bind`]'s shadow branch and the 42N66 rule read the same predicate.
pub(crate) fn shadowable_imports(env: &Env) -> std::collections::HashSet<String> {
    env.iter()
        .filter(|(_, binding)| binding.is_non_entity())
        .map(|(name, _)| name.clone())
        .collect()
}

/// Whether a NEW pattern binding of `name` collides with the scope. A
/// shadowable non-entity import is *fresh* for a pattern binding (the
/// pattern rebinds it), so it does not count as already bound — the binder's
/// own answer, shared with the 42N66 rule so the two cannot disagree.
pub(crate) fn effectively_bound(
    env: &Env,
    shadowable: &std::collections::HashSet<String>,
    name: &SmallName,
) -> bool {
    match env.get_name(name) {
        None => false,
        Some(binding) => {
            !(name.with_str(|text| shadowable.contains(text)) && binding.is_non_entity())
        }
    }
}

pub(crate) type Env = HashMap<String, Binding>;

/// The Env's IR-name accessors: the binding environment stays String-keyed
/// (its `next_stage_scope` feeds come from the String-rendered output-name
/// computation), so IR names — `SmallName`, ADR-0509 stage 3 — reach it
/// through these three bridges. Reads are allocation-free (the inline arm
/// decodes on the stack); the one insert bridge renders text per bound
/// name, exactly the allocation the pre-SmallName clone paid.
/// The three Env bridges, as a trait so the methods live beside the type
/// alias they serve without a wrapper struct.
pub(crate) trait EnvBridge {
    fn get_name(&self, name: &SmallName) -> Option<&Binding>;
    fn contains_name(&self, name: &SmallName) -> bool;
    /// Like [`HashMap::insert`]: the binding the name replaced, if any.
    fn insert_name(&mut self, name: SmallName, binding: Binding) -> Option<Binding>;
}

impl EnvBridge for HashMap<String, Binding> {
    fn get_name(&self, name: &SmallName) -> Option<&Binding> {
        name.with_str(|text| self.get(text))
    }
    fn contains_name(&self, name: &SmallName) -> bool {
        name.with_str(|text| self.contains_key(text))
    }
    fn insert_name(&mut self, name: SmallName, binding: Binding) -> Option<Binding> {
        self.insert(name.to_string(), binding)
    }
}

/// The scope a `NEXT` stage starts from: the chain's inherited environment
/// and name order with the previous stage's exported columns added
/// (CIP-213 — an imported name stays visible in every statement of the
/// chain, a stage's own columns from the stage after it). Shared by the
/// planner's stage recursion and the validation driver's, so the seeding
/// rule has one spelling.
pub(crate) fn next_stage_scope(
    seed_env: &Env,
    seed_names: &[String],
    output_names: &[String],
) -> (Env, Vec<String>) {
    let mut env = seed_env.clone();
    let mut names = seed_names.to_vec();
    for name in output_names {
        env.insert(name.clone(), Binding::Value);
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    (env, names)
}

/// Bind one name. `shadowable` names the bindings a MATCH pattern may
/// *shadow* instead of colliding with — a subquery-expression body's
/// imported *non-entity* names, which the body's own pattern may rebind
/// (`EXISTS { (n) … }` where the enclosing scope bound `n` to a scalar).
/// CREATE/MERGE shadowing is [`bind_create_path`]'s own policy.
pub(crate) fn bind(
    env: &mut Env,
    name: &SmallName,
    kind: Binding,
    create: bool,
    shadowable: &std::collections::HashSet<String>,
) -> Result<()> {
    if create && env.contains_name(name) {
        return Err(name.with_str(Error::duplicate_variable));
    }
    // A shadowable non-entity import is a fresh binding inside this scope
    // ([`effectively_bound`]'s answer): the pattern's own declaration
    // replaces it, whichever entity kind it is (a node, a relationship, or
    // a named path). An unbound name is fresh for the same reason.
    if !effectively_bound(env, shadowable, name) {
        env.insert_name(name.clone(), kind);
        return Ok(());
    }
    // A path may not rebind an existing name at all (a path is never the
    // reuse shape: its element variables were bound moments ago).
    if kind == Binding::Path {
        return Err(name.with_str(Error::duplicate_variable));
    }
    match env.get_name(name).copied() {
        None => {
            env.insert_name(name.clone(), kind);
            Ok(())
        }
        Some(prev) if prev == kind => Ok(()),
        // A list of relationships may be reused as a var-length pattern.
        Some(Binding::List) if kind == Binding::Rel => Ok(()),
        // The unconstrained `Value` adopts any concrete kind.
        Some(Binding::Value) => {
            env.insert_name(name.clone(), kind);
            Ok(())
        }
        Some(_) => Err(variable_type_conflict(name)),
    }
}

/// The one spelling of a reused-name type clash (`VariableTypeConflict`).
fn variable_type_conflict(name: &SmallName) -> Error {
    Error::syntax(format!(
        "VariableTypeConflict: `{name}` already bound to a different type"
    ))
}

pub(crate) fn bind_path(
    env: &mut Env,
    p: &PathPattern,
    create: bool,
    shadowable: &std::collections::HashSet<String>,
) -> Result<()> {
    // openCypher TCK `clauses/match/Match3.feature` #29 (the language floor):
    // a relationship variable reused in a pattern that closes back onto a
    // repeated node variable is a compile-time RelationshipUniquenessViolation.
    if !create
        && p.nodes
            .first()
            .and_then(|node| node.variable.as_ref())
            .zip(p.nodes.last().and_then(|node| node.variable.as_ref()))
            .is_some_and(|(first, last)| first == last)
    {
        let mut rels = std::collections::HashSet::new();
        if p.rels
            .iter()
            .filter_map(|rel| rel.variable.as_ref())
            .any(|name| !rels.insert(name))
        {
            return Err(Error::syntax(
                "RelationshipUniquenessViolation: a relationship variable cannot be reused in a closed legacy pattern",
            ));
        }
    }
    for (name, binding) in pattern_local_bindings(p) {
        // A non-var-length relationship variable reusing a name already
        // bound to a list is a type clash (the var-length reuse is the
        // legal shape; `bind` admits it below).
        if binding == Binding::Rel
            && effectively_bound(env, shadowable, name)
            && env.get_name(name) == Some(&Binding::List)
        {
            return Err(variable_type_conflict(name));
        }
        bind(env, name, binding, create, shadowable)?;
    }
    Ok(())
}

/// The local variable bindings one pattern introduces, in declaration
/// order — the single classification table behind [`bind_path`]
/// (MATCH/CREATE patterns) and the expression checker's
/// pattern-comprehension scope: a path name is a path, a node variable a
/// node, and a relationship variable a relationship — or a list when it is
/// var-length or inside a quantified path, whose subpath variable is a
/// list too. GPM `elements` are classified only for the pure element form
/// (legacy `nodes`/`rels` empty), the same shape `bind_path` binds.
pub(crate) fn pattern_local_bindings(p: &PathPattern) -> Vec<(&SmallName, Binding)> {
    let mut out = Vec::new();
    for name in p.path_names() {
        out.push((name, Binding::Path));
    }
    for n in &p.nodes {
        if let Some(v) = &n.variable {
            out.push((v, Binding::Node));
        }
    }
    for r in &p.rels {
        if let Some(v) = &r.variable {
            out.push((
                v,
                if r.var_length {
                    Binding::List
                } else {
                    Binding::Rel
                },
            ));
        }
    }
    if p.nodes.is_empty() && p.rels.is_empty() {
        collect_element_bindings(&p.elements, false, &mut out);
    }
    out
}

/// The `elements` half of [`pattern_local_bindings`]: a node or
/// relationship inside a quantified path (`grouped`) binds a list, and a
/// quantified subpath's own variable is a list.
fn collect_element_bindings<'a>(
    elements: &'a [PatternElement],
    grouped: bool,
    out: &mut Vec<(&'a SmallName, Binding)>,
) {
    for element in elements {
        match element {
            PatternElement::Node(node) => {
                if let Some(name) = &node.variable {
                    out.push((
                        name,
                        if grouped {
                            Binding::List
                        } else {
                            Binding::Node
                        },
                    ));
                }
            }
            PatternElement::Relationship(rel) => {
                if let Some(name) = &rel.variable {
                    out.push((
                        name,
                        if grouped || rel.var_length {
                            Binding::List
                        } else {
                            Binding::Rel
                        },
                    ));
                }
            }
            PatternElement::Quantified(qpp) => {
                collect_element_bindings(&qpp.elements, true, out);
                if let Some(name) = &qpp.subpath_variable {
                    out.push((name, Binding::List));
                }
            }
        }
    }
}

/// Declare one CREATE-introduced binding. A shadowable name — one an
/// enclosing scope bound and a FOREACH body may rebind — is always fresh;
/// otherwise a name already in scope is a collision unless the caller's
/// shape rule says it is a *reuse* of the existing binding
/// (`may_reuse`), in which case the binding is left exactly as it was.
fn declare(
    env: &mut Env,
    name: &SmallName,
    kind: Binding,
    shadowable: &std::collections::HashSet<String>,
    may_reuse: bool,
) -> Result<()> {
    if name.with_str(|text| shadowable.contains(text)) {
        env.insert_name(name.clone(), kind);
        return Ok(());
    }
    if env.contains_name(name) {
        if may_reuse {
            return Ok(());
        }
        return Err(name.with_str(Error::duplicate_variable));
    }
    env.insert_name(name.clone(), kind);
    Ok(())
}

pub(crate) fn bind_create_path(
    env: &mut Env,
    p: &PathPattern,
    shadowable: &std::collections::HashSet<String>,
) -> Result<()> {
    if let Some(v) = &p.variable {
        declare(env, v, Binding::Path, shadowable, false)?;
    }
    // A plain variable node in a rel-bearing path (`(a)-[:R]->(b)` where the
    // pattern reuses an earlier element variable) is a reuse, not a
    // redeclaration; adding a label or a property spec makes it a
    // declaration again.
    let node_may_reuse = !p.rels.is_empty();
    for n in &p.nodes {
        if let Some(v) = &n.variable {
            declare(
                env,
                v,
                Binding::Node,
                shadowable,
                node_may_reuse && n.labels.is_empty() && n.properties.is_none(),
            )?;
        }
    }
    for r in &p.rels {
        if let Some(v) = &r.variable {
            declare(
                env,
                v,
                Binding::Rel,
                shadowable,
                r.types.is_empty() && r.properties.is_none(),
            )?;
        }
    }
    Ok(())
}
