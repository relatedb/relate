use super::*;

/// The property spec and element `WHERE` read after a node or relationship
/// element's variable and label/type spec.
#[derive(Default)]
struct ElementTail {
    properties: Vec<(SmallName, Expr)>,
    has_properties: bool,
    properties_param: Option<Expr>,
    where_expr: Option<Expr>,
}

impl<'a> Parser<'a> {
    // ---- Pattern parsing ----

    pub(super) fn parse_pattern(&mut self) -> ParseResult<Vec<PatternPath>> {
        let mut paths = Vec::new();
        loop {
            paths.push(self.parse_pattern_path()?);
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        Ok(paths)
    }

    /// True when the token `offset` ahead is a symbolic name directly
    /// followed by `=` -- the named-path prefix `name = ...` (ADR-0191: a
    /// keyword spelling works too). The callers pass `offset == 0` at a
    /// pattern position and `offset == 1` past a leading `(` (the
    /// parenthesized subpath group).
    pub(super) fn peek_named_path_prefix(&self, offset: usize) -> bool {
        self.peek_at(offset).ty.is_schema_name() && self.peek_at(offset + 1).ty == TokenType::Equals
    }

    pub(super) fn parse_pattern_path(&mut self) -> ParseResult<PatternPath> {
        // Optional named-path prefix: `p = ...`. Consumed up front so that a
        // path variable assigned to a top-level parenthesized path pattern
        // (`p = ((a)-[r]->(b))`, CIP-19 basic-structure #15) is recognized
        // before `parse_pattern_path_body`'s own inner double-paren handling.
        // The variable is a symbolic name, so any keyword spelling works
        // unquoted too (ADR-0191).
        let outer_var = if self.peek_named_path_prefix(0) {
            let tok = self.next_token(); // consume the path variable
            self.next_token(); // consume =
            Some(SmallName::from_name(&tok.literal))
        } else {
            None
        };
        self.parse_pattern_path_named(outer_var)
    }

    fn parse_pattern_path_named(
        &mut self,
        outer_var: Option<SmallName>,
    ) -> ParseResult<PatternPath> {
        // Pre-GPM `shortestPath(...)`/`allShortestPaths(...)` used as a
        // top-level MATCH pattern is one reader of its own
        // ([`Self::try_parse_shortest_path_function`]), so this function
        // stays a linear read: sugar-or-general body, the one shared
        // outer-variable bind, the tail.
        let mut path = match self.try_parse_shortest_path_function()? {
            Some(path) => path,
            None => {
                let (selector, mode) = self.parse_path_prefix()?;

                // The path variable declaration
                // must precede the mode/selector prefix. If another `name =`
                // follows the prefix we just consumed, the input was actually
                // `MODE p = ...` (mode before the variable) -- reject it
                // explicitly rather than silently reinterpreting it. A
                // keyword spelling is a path variable too (ADR-0191), so it
                // must be rejected here the same way.
                if self.peek_named_path_prefix(0) {
                    return Err(self.error(
                        "InvalidPathModePosition: the path variable declaration must precede the path mode/selector prefix",
                    ));
                }

                // The 42N42 sub-path-binding rule, resolved once
                // (`subpath_binding_allowed`) and threaded through the body
                // reader unchanged.
                let allowed = subpath_binding_allowed(outer_var.is_some(), selector.as_ref());
                let mut path = self.parse_pattern_path_body(allowed)?;
                path.mode = mode;
                path.selector = selector;
                path
            }
        };
        // A named parenthesized group that is the whole body
        // (`p = ANY SHORTEST (q = (:A)-->+(:B))`) keeps both names: the
        // element group already put `q` in `variable`, and the enclosing
        // path name `p` binds the same path value. Reusing the enclosing
        // path's own name for the group is Neo4j's 42N59 duplicate
        // declaration, not a silent collapse to one name.
        if let Some(var) = outer_var {
            self.bind_outer_path_variable(&mut path, var)?;
        }
        self.finish_pattern_path(path)
    }

    /// Binds the path's outer `<name> =` variable — the declaration
    /// `parse_pattern_path` consumed before the body (or a later path of
    /// the comma list carries). A body that already bound its own name
    /// keeps it as the path's subpath variable, unless it reuses the
    /// outer name (Neo4j's 42N59 duplicate declaration, never a silent
    /// collapse to one name). The one bind for both named-path readers
    /// in [`Self::parse_pattern_path_named`]; the two duplicate checks
    /// stay separate on purpose — the second reads the subpath slot
    /// *after* the inner name moved into it, so folding them into one
    /// pre-assignment guard would change what `p = q = (p = ...)`
    /// rejects. Both raise the one error
    /// [`Self::variable_already_defined`] authors.
    fn bind_outer_path_variable(&self, path: &mut PatternPath, var: SmallName) -> ParseResult<()> {
        if let Some(inner) = path.variable.take() {
            if inner == var {
                return Err(self.variable_already_defined(&var));
            }
            path.subpath_variable = Some(inner);
        }
        if path.subpath_variable.as_ref() == Some(&var) {
            return Err(self.variable_already_defined(&var));
        }
        path.variable = Some(var);
        Ok(())
    }

    /// The one construction of the 42N59 duplicate-declaration parse
    /// error — the message is a single fact, authored once for every
    /// binder that rejects a reused name.
    fn variable_already_defined(&self, var: &SmallName) -> crate::error::ParseError {
        self.error(format!(
            "VariableAlreadyDefined: variable `{var}` already declared"
        ))
    }

    /// The pre-GPM `shortestPath(...)`/`allShortestPaths(...)` function
    /// sugar as a top-level MATCH pattern: `SHORTEST 1 PATH (...)` /
    /// `ALL SHORTEST (...)` respectively. `Some(path)` when the pattern
    /// at the cursor is the function form, its tail fields set and the
    /// outer-variable bind left to the caller (the one bind both arms
    /// share); `None` when it is not, with no token consumed -- the
    /// caller proceeds to the general prefix-and-body read. Recognition
    /// rides the canonical contextual-word probes -- no lowered copy, no
    /// separate `Identifier` guard -- with the one `(` lookahead spelled
    /// as the raw token-type check the crate's other readers use. The
    /// 42N42 rule resolves the sugar arm's
    /// body directly: the function form's selector is always selective,
    /// so sub-path binding stays allowed whatever the enclosing path --
    /// `subpath_binding_allowed(true)` at its one reader. A quantified
    /// path pattern inside the function is the authored rejection, not a
    /// silent partial sugar.
    fn try_parse_shortest_path_function(&mut self) -> ParseResult<Option<PatternPath>> {
        let selector = if self.peek_at_is_ident(0, "allshortestpaths") {
            PathSelector::AllShortest
        } else if self.peek_at_is_ident(0, "shortestpath") {
            PathSelector::Shortest {
                count: Some(PathCount::Literal(1)),
                group: false,
            }
        } else {
            return Ok(None);
        };
        if self.peek_at(1).ty != TokenType::LeftParen {
            return Ok(None);
        }
        self.next_token(); // consume shortestPath/allShortestPaths
        self.next_token(); // consume (
        let mut path =
            self.parse_pattern_path_body(subpath_binding_allowed(true, Some(&selector)))?;
        self.expect(TokenType::RightParen)?;
        if contains_gpm_quantification(&path.elements) {
            return Err(self.error(
                "QuantifiedPathPatternInShortestPath: a quantified path pattern cannot be used inside shortestPath()/allShortestPaths()",
            ));
        }
        path.mode = None;
        path.selector = Some(selector);
        path.is_shortest_path_function = true;
        Ok(Some(path))
    }

    /// Shared tail of `parse_pattern_path`: the rule that a
    /// top-level path pattern must have a minimum node count > 0 -- it
    /// cannot consist entirely of zero-lower-bound quantified path patterns
    /// (which could all collapse to zero iterations, leaving no node at
    /// all). A plain `Node` element always guarantees at least one node; a
    /// `Quantified` element guarantees one only if its own quantifier
    /// requires at least one iteration.
    pub(super) fn finish_pattern_path(&self, path: PatternPath) -> ParseResult<PatternPath> {
        let has_guaranteed_node = path.elements.iter().any(|e| match e {
            PatternElement::Node(_) => true,
            PatternElement::Quantified(q) => q.quantifier.min >= 1,
            PatternElement::Relationship(_) => false,
        });
        if !has_guaranteed_node {
            return Err(self.error(
                "MinimumNodeCountZero: a path pattern must have a minimum node count greater than zero",
            ));
        }

        Ok(path)
    }

    /// Parses one path pattern's body: an optional named-path prefix
    /// (`ident = `, recognized both at the top level and for a subpath
    /// variable inside a quantification, e.g. `(p2 = (x)-->(y))*`) followed
    /// by a sequence of node, relationship, and quantified-path-pattern
    /// factors (by concatenation). Node and quantified
    /// factors may be juxtaposed directly (no relationship between them,
    /// e.g. `(x)((a)-->(b))*` or `((a)-->(b))*((c)-->(d))*`); a relationship
    /// factor must always be preceded by an already-parsed element and
    /// followed by a node or quantified factor.
    ///
    /// `subpath_binding_allowed` is the 42N42 sub-path-binding rule
    /// resolved by the enclosing reader (the path is named, or the search
    /// is selective); the body reader forwards it unchanged to the group
    /// arm's reader, which consumes it.
    pub(super) fn parse_pattern_path_body(
        &mut self,
        subpath_binding_allowed: bool,
    ) -> ParseResult<PatternPath> {
        let mut path = PatternPath {
            variable: None,
            subpath_variable: None,
            elements: Vec::new(),
            where_expr: None,
            mode: None,
            selector: None,
            is_shortest_path_function: false,
        };

        // Check for named path: symbolic name = (...) -- a keyword spelling
        // included (ADR-0191), e.g. a subpath variable inside a quantified
        // path pattern (`(MATCH = (x)-->(y))*`).
        if self.peek_named_path_prefix(0) {
            let tok = self.next_token();
            self.next_token(); // consume =
            path.variable = Some(SmallName::from_name(&tok.literal));
        }

        // A match-mode phrase (`REPEATABLE ELEMENTS` / `DIFFERENT
        // RELATIONSHIPS`) can never start a path pattern body: the one
        // legal position is directly after `MATCH`, where
        // `parse_match_mode_prefix` consumes it before the pattern is
        // parsed. Neo4j rejects the phrase everywhere else in the pattern
        // (`MATCH p = REPEATABLE ELEMENTS (...)`, a comma-separated later
        // path, a `shortestPath(...)` argument, an `OPTIONAL MATCH`
        // alike), so the shared body boundary every pattern reader
        // crosses — the named path, the `shortestPath()` sugar, a
        // parenthesized group — rejects it with the authored position
        // error instead of the generic expected-paren message. The phrase
        // is not a *path* mode (`WALK`/`TRAIL`/`ACYCLIC`/`SIMPLE`,
        // legitimately prefixable per CIP-51) — `parse_path_prefix`
        // leaves the cursor on it.
        if self.peek_match_mode_at(0).is_some() {
            return Err(self.error(
                "InvalidPathModePosition: the match mode phrase must follow MATCH, not a path pattern",
            ));
        }

        loop {
            let ty = self.peek_type();
            if !ty.starts_pattern_element() {
                break;
            }
            match ty {
                TokenType::LeftParen if self.peek_is_parenthesized_subpath_start() => {
                    // Parenthesized subpath group (CIP-14): quantified, it
                    // is one more element; unquantified, it is the whole
                    // path.
                    if self.parse_parenthesized_subpath(subpath_binding_allowed, &mut path)? {
                        return Ok(path);
                    }
                }
                TokenType::LeftParen => {
                    let node = self.parse_node_pattern()?;
                    path.elements.push(PatternElement::Node(node));
                }
                // The continuation guard above leaves only relationship
                // heads here.
                _ => {
                    if path.elements.is_empty() {
                        return Err(self.error("a pattern must start with a node pattern"));
                    }
                    let rel = self.parse_rel_pattern()?;
                    path.elements.push(PatternElement::Relationship(rel));
                    if self.peek_type() != TokenType::LeftParen {
                        return Err(self.error("expected node pattern after relationship"));
                    }
                }
            }
        }

        if path.elements.is_empty() {
            return Err(self.error("expected a pattern"));
        }

        Ok(path)
    }

    /// True if the parser is positioned at the start of a parenthesized
    /// subpath group (CIP-14 `((...))`) -- either the plain `((` form, or
    /// Consumes the current token if it is the contextual identifier `s`
    /// (see `peek_is_ident`); returns whether it did. Used for the
    /// CIP-51/CIP-73/CIP-191 contextual keywords (WALK, TRAIL, ACYCLIC,
    /// SIMPLE, ANY, SHORTEST, GROUP, GROUPS, PATH, PATHS), which stay
    /// ordinary identifiers everywhere else in the grammar (so they never
    /// shadow the `all()`/`any()`/`none()`/`single()` predicate functions or
    /// a variable of the same name).
    pub(super) fn consume_ident(&mut self, s: &str) -> bool {
        if self.peek_is_ident(s) {
            self.next_token();
            true
        } else {
            false
        }
    }

    /// Parses an optional CIP-51 path mode keyword (`WALK`/`TRAIL`/
    /// `ACYCLIC`/`SIMPLE`).
    pub(super) fn parse_path_mode_keyword(&mut self) -> Option<PathMode> {
        if self.consume_ident("walk") {
            Some(PathMode::Walk)
        } else if self.consume_ident("trail") {
            Some(PathMode::Trail)
        } else if self.consume_ident("acyclic") {
            Some(PathMode::Acyclic)
        } else if self.consume_ident("simple") {
            Some(PathMode::Simple)
        } else {
            None
        }
    }

    /// Consumes an optional CIP-51/CIP-73 `PATH`/`PATHS` noise keyword
    /// (`<path keywords>` / `<path or paths>`); has no semantic effect.
    pub(super) fn consume_path_or_paths_noise(&mut self) {
        if !self.consume_ident("path") {
            self.consume_ident("paths");
        }
    }

    /// Parses the `<number of paths>`/`<number of groups>`
    /// non-terminal, extended by CIP-191's `<dynamic parameter
    /// specification>`: an unsigned integer literal (validated `> 0` here)
    /// or a bare `$parameter` (validated at runtime, once its value
    /// is known). Returns `None` if neither is present.
    pub(super) fn parse_number_of_paths(&mut self) -> ParseResult<Option<PathCount>> {
        if self.peek_type() == TokenType::Integer {
            let tok = self.next_token();
            // Checked conversion via the shared bound helper, not a silent
            // clamp (LABS-81): `ANY 4294967296 PATHS` used to wrap to
            // selector 0.
            let n = self.parse_u32_bound(&tok, "InvalidNumberOfPaths: number of paths/groups")?;
            if n == 0 {
                return Err(self.error_at_token(
                    &tok,
                    "InvalidNumberOfPaths: the number of paths/groups must be greater than 0",
                ));
            }
            return Ok(Some(PathCount::Literal(n)));
        }
        if self.peek_type() == TokenType::Parameter {
            let tok = self.next_token();
            return Ok(Some(PathCount::Parameter(tok.literal.into_owned())));
        }
        Ok(None)
    }

    /// Parses the optional CIP-51 path-mode-only prefix or CIP-73/CIP-191
    /// path-search (selector) prefix that may precede a path pattern's body
    /// (`<path pattern prefix>`). Returns
    /// `(selector, mode)`; both `None` when neither is written.
    pub(super) fn parse_path_prefix(
        &mut self,
    ) -> ParseResult<(Option<PathSelector>, Option<PathMode>)> {
        if self.peek_type() == TokenType::ALL {
            self.next_token(); // consume ALL
            if self.consume_ident("shortest") {
                let mode = self.parse_path_mode_keyword();
                self.consume_path_or_paths_noise();
                return Ok((Some(PathSelector::AllShortest), mode));
            }
            let mode = self.parse_path_mode_keyword();
            self.consume_path_or_paths_noise();
            return Ok((Some(PathSelector::All), mode));
        }
        if self.peek_is_ident("any") {
            self.next_token(); // consume ANY
            if self.consume_ident("shortest") {
                let mode = self.parse_path_mode_keyword();
                self.consume_path_or_paths_noise();
                return Ok((Some(PathSelector::AnyShortest), mode));
            }
            let count = self.parse_number_of_paths()?;
            let mode = self.parse_path_mode_keyword();
            self.consume_path_or_paths_noise();
            return Ok((Some(PathSelector::Any(count)), mode));
        }
        if self.peek_is_ident("shortest") {
            self.next_token(); // consume SHORTEST
            let count = self.parse_number_of_paths()?;
            let mode = self.parse_path_mode_keyword();
            self.consume_path_or_paths_noise();
            let group = self.consume_ident("group") || self.consume_ident("groups");
            if !group && count.is_none() {
                return Err(self.error(
                    "InvalidNumberOfPaths: SHORTEST requires a number of paths unless followed by GROUP/GROUPS",
                ));
            }
            return Ok((Some(PathSelector::Shortest { count, group }), mode));
        }
        // No selector: a bare path-mode-only prefix, if any.
        let mode = self.parse_path_mode_keyword();
        if mode.is_some() {
            self.consume_path_or_paths_noise();
        }
        Ok((None, mode))
    }

    /// `(name = (` when the group itself carries a subpath variable
    /// (e.g. `(p2 = (x)-->(y))*`). Distinguishes this
    /// from a plain node pattern, whose own label/property spec never
    /// starts with `(`. The subpath variable is a symbolic name, so any
    /// keyword spelling works unquoted too (ADR-0191).
    pub(super) fn peek_is_parenthesized_subpath_start(&self) -> bool {
        if self.peek_type() != TokenType::LeftParen {
            return false;
        }
        if self.peek_at(1).ty == TokenType::LeftParen {
            return true;
        }
        self.peek_named_path_prefix(1) && self.peek_at(3).ty == TokenType::LeftParen
    }

    /// Parses a CIP-28 quantifier: `*`, `+`, `{m,n}`, `{m,}`, `{,n}`, `{,}`,
    /// or `{n}`. The current token must satisfy
    /// [`TokenType::is_quantifier_start`]. Marks the GPM quantifier form
    /// on the clause's mixing flags at entry -- both call sites (the
    /// parenthesized-subpath group and the relationship pattern's
    /// postfix quantifier) are the GPM/QPP syntax, mirroring
    /// `parse_legacy_hop_bounds`'s own flag. Applies the `{,}` -> `{0,}`
    /// normalization (an absent lower bound defaults to 0) and the bounds
    /// validation (upper bound > 0 if present, lower bound <= upper
    /// bound).
    pub(super) fn parse_quantifier(&mut self) -> ParseResult<PathQuantifier> {
        self.side_effects.quantifiers.gpm_form = true;
        match self.peek_type() {
            TokenType::Star => {
                self.next_token();
                Ok(PathQuantifier { min: 0, max: None })
            }
            TokenType::Plus => {
                self.next_token();
                Ok(PathQuantifier { min: 1, max: None })
            }
            TokenType::LeftBrace => {
                self.next_token(); // consume {
                let lower = if self.peek_type() == TokenType::Integer {
                    let tok = self.next_token();
                    Some(self.parse_u32_bound(&tok, "quantifier lower bound")?)
                } else {
                    None
                };
                let (min, max) = if self.peek_type() == TokenType::Comma {
                    self.next_token(); // consume ,
                    let upper = if self.peek_type() == TokenType::Integer {
                        let tok = self.next_token();
                        Some(self.parse_u32_bound(&tok, "quantifier upper bound")?)
                    } else {
                        None
                    };
                    (lower.unwrap_or(0), upper)
                } else {
                    // No comma: a fixed quantifier `{n}`, equivalent to `{n,n}`.
                    let n = lower
                        .ok_or_else(|| self.error("expected an integer or ',' in quantifier"))?;
                    (n, Some(n))
                };
                self.expect(TokenType::RightBrace)?;
                if let Some(upper) = max {
                    if upper == 0 {
                        return Err(self.error(
                            "ZeroUpperBound: a quantifier's upper bound must be greater than zero",
                        ));
                    }
                    if min > upper {
                        return Err(self.error(
                            "LowerBoundGreaterThanUpperBound: a quantifier's lower bound cannot exceed its upper bound",
                        ));
                    }
                }
                Ok(PathQuantifier { min, max })
            }
            _ => Err(self.error("expected a quantifier")),
        }
    }

    /// ADR-0191 Decision point 4: true when the token `offset` ahead can be
    /// read as a node or relationship pattern's element variable. Only a
    /// symbolic name (`is_schema_name()`: an `Identifier` or any keyword)
    /// can be one; the `WHERE` and `IS` spellings additionally keep their
    /// existing meanings in front of the variable read, decided by a
    /// one-token lookahead:
    ///
    /// - a leading `WHERE` is the variable only when the next token cannot
    ///   start the inline element predicate (`)`, `]`, `:`, `IS`, `$`, `-`,
    ///   `<-`, `WHERE`); otherwise the inline-predicate meaning wins, so
    ///   `(WHERE 1 = 1)` stays an anonymous element with a predicate while
    ///   `(WHERE)` and `(where WHERE true)` read `WHERE` as the variable.
    ///   The `-`/`<-` entries are the fused relationship spellings of
    ///   ADR-0191's recorded set; a spaced left head never reaches this
    ///   lookahead — it can only continue a path *after* the element
    ///   closes, so `MATCH (WHERE) < - [:R] - (b)` reads `WHERE` as the
    ///   variable through the `)` entry (the same reading live Neo4j
    ///   2026.10 gives it), and the expression-context claim of a spaced
    ///   head stays the deferred split recorded on
    ///   `TokenType::is_relationship_start`;
    /// - a leading `IS` is the variable only when the next token cannot
    ///   start a label spec; otherwise the variable-less GPM
    ///   label-introducer meaning wins, so `(IS)`/`[IS]` read `IS` as the
    ///   variable while `(IS A&B)`/`-[IS R]->` keep `IS` as the introducer.
    ///   The label-spec test is the shared offset-aware
    ///   `peek_can_start_label_expr_at`, so this lookahead and the
    ///   expression-level label reader cannot drift apart.
    ///
    /// Also used by the pattern-comprehension/predicate lookahead
    /// (`node_pattern_precedes_relationship`), which must skip exactly the
    /// tokens this reader would consume as the variable.
    pub(super) fn peek_pattern_name_is_variable(&self, offset: usize) -> bool {
        let ty = self.peek_at(offset).ty;
        if !ty.is_schema_name() {
            return false;
        }
        match ty {
            TokenType::WHERE => matches!(
                self.peek_at(offset + 1).ty,
                TokenType::RightParen
                    | TokenType::RightBracket
                    | TokenType::Colon
                    | TokenType::IS
                    | TokenType::Parameter
                    | TokenType::Dash
                    | TokenType::ArrowLeft
                    | TokenType::WHERE
            ),
            TokenType::IS => !self.peek_can_start_label_expr_at(offset + 1),
            _ => true,
        }
    }

    /// Parses the tail shared by node and relationship elements: an optional
    /// property spec -- a `{...}` map literal, or a bare `$param` (the
    /// whole-map shorthand) -- followed by an optional element `WHERE`.
    fn parse_element_property_and_where(&mut self) -> ParseResult<ElementTail> {
        let mut tail = ElementTail::default();
        if self.peek_type() == TokenType::LeftBrace {
            tail.has_properties = true;
            let pairs = self.parse_map_literal()?;
            tail.properties = pairs.into_iter().map(|p| (p.key, p.value)).collect();
        } else if self.peek_type() == TokenType::Parameter {
            tail.has_properties = true;
            let tok = self.next_token();
            tail.properties_param = Some(Expr::Parameter(tok.literal.into_owned()));
        }
        if self.peek_type() == TokenType::WHERE {
            self.next_token();
            tail.where_expr = Some(self.parse_expression()?);
        }
        Ok(tail)
    }

    pub(super) fn parse_node_pattern(&mut self) -> ParseResult<NodePattern> {
        // The probe's tree cache: a tree parked at this `(` position by
        // a speculative probe is taken instead of re-parsed — the kit's
        // take replays the position-after and the side-effect delta (see
        // `parser/speculation.rs` for the insert/take policy; this is
        // what keeps the reader-driven gate linear on adversarial
        // nested shapes: the probe's descent builds each nested tree
        // once and every enclosing parse moves it up, instead of each
        // gate re-descending its whole subtree).
        if let Some(node) = self.take_cached_node_pattern() {
            return Ok(node);
        }
        let cache_pos = self.pos;
        let flags_before = self.side_effects;

        self.expect(TokenType::LeftParen)?;
        let mut node = NodePattern {
            variable: None,
            label_expr: None,
            properties: Vec::new(),
            has_properties: false,
            properties_param: None,
            where_expr: None,
        };

        let tok = self.peek().clone();
        // The element variable is a symbolic name (ADR-0191), so any
        // keyword spelling works unquoted. `WHERE` and `IS` are read as the
        // variable only when the one-token lookahead shows their other
        // meaning cannot apply (`peek_pattern_name_is_variable`, which folds
        // in the symbolic-name check): a leading `WHERE` otherwise starts
        // the inline element predicate and a leading `IS` otherwise starts a
        // variable-less GPM label spec (`(IS A&B)`).
        if self.peek_pattern_name_is_variable(0) {
            node.variable = Some(SmallName::from_name(&tok.literal));
            self.next_token();
        }

        if matches!(self.peek_type(), TokenType::Colon | TokenType::IS) {
            // An element's own label chain: a `{` after it is the
            // element's inline property map, never a map projection, so
            // the comprehension pipe-ambiguity rule must not treat it as
            // a dead end here (see `chain_tail_ends_where`). The scoped
            // guard keeps the depth balanced across the `?`.
            let spec = self.with_pattern_element_depth(|p| p.parse_label_spec(false))?;
            node.label_expr = Some(spec.expr);
        }

        // Property spec, then the element-pattern WHERE: `(var:LE {props} WHERE ...)`
        // (CIP-17).
        let tail = self.parse_element_property_and_where()?;
        node.has_properties = tail.has_properties;
        node.properties = tail.properties;
        node.properties_param = tail.properties_param;
        node.where_expr = tail.where_expr;

        self.expect(TokenType::RightParen)?;
        self.park_node_pattern(cache_pos, flags_before);
        Ok(node)
    }

    /// Parses the legacy bracket-syntax variable-length hop bounds
    /// (`*min..max`, including a bare `*`) of a relationship pattern; the
    /// current token must be `Star`. Returns `(min, max)` -- an absent
    /// upper bound is `None`, a bare `*` is `(1, None)`, and a single
    /// number `*n` is the exact hop count `(n, Some(n))`. Sets
    /// `quantifiers.legacy_bracket` for the clause-level
    /// quantifier-mixing check.
    fn parse_legacy_hop_bounds(&mut self) -> ParseResult<(u32, Option<u32>)> {
        debug_assert_eq!(self.peek_type(), TokenType::Star);
        self.side_effects.quantifiers.legacy_bracket = true;
        self.next_token();
        let lower = if self.peek_type() == TokenType::Integer {
            let tok = self.next_token();
            Some(self.parse_u32_bound(&tok, "relationship hop lower bound")?)
        } else {
            None
        };
        if self.peek_type() != TokenType::DotDot {
            // `*` (unbounded) or `*n` (exact hop count).
            return Ok(lower.map_or((1, None), |n| (n, Some(n))));
        }
        self.next_token();
        let upper = if self.peek_type() == TokenType::Integer {
            let tok = self.next_token();
            Some(self.parse_u32_bound(&tok, "relationship hop upper bound")?)
        } else {
            None
        };
        Ok((lower.unwrap_or(1), upper))
    }

    /// Reads a relationship pattern's two arrowheads -- the left before
    /// the optional bracket body (filled into `rel` through
    /// [`Parser::parse_rel_bracket_body`]), the right after it -- and
    /// returns `(left_arrow, right_head)` for the direction matrix. All
    /// the head spellings live here, and only here:
    ///
    /// - the left head is a plain dash, the fused `<-`, or the spaced
    ///   `< -` (LessThan then the head dash). The spaced spelling is a
    ///   pattern-body form only -- in an expression context a `<` after a
    ///   `)` keeps its comparison meaning (`RETURN (a) < -1`), so the
    ///   pattern-predicate/comprehension gates
    ///   (`is_relationship_start`) stay on the fused pair;
    /// - the right head is written either glued to the tail dash (`->`,
    ///   one token) or spaced from it (`- >`, Dash then GreaterThan): the
    ///   spaced head is consumed where it can only be the arrowhead --
    ///   after a relationship tail a path requires a node pattern, so a
    ///   comparison `>` never reaches this position (those are read
    ///   inside the bracket WHERE, or in the clause after the pattern
    ///   ends).
    fn read_relationship_heads(&mut self, rel: &mut RelPattern) -> ParseResult<(bool, bool)> {
        let tok = self.next_token();
        let left_arrow = match tok.ty {
            TokenType::ArrowLeft => true,
            TokenType::Dash => false,
            TokenType::LessThan => {
                let dash = self.next_token();
                if dash.ty != TokenType::Dash {
                    return Err(self.error_at_token(
                        &dash,
                        format!("expected - after <, got {:?}", dash.literal),
                    ));
                }
                true
            }
            _ => {
                return Err(
                    self.error_at_token(&tok, format!("expected - or <-, got {:?}", tok.literal))
                );
            }
        };

        if self.peek_type() == TokenType::LeftBracket {
            self.parse_rel_bracket_body(rel)?;
        }

        let right_tok = self.next_token();
        let right_head = match right_tok.ty {
            TokenType::ArrowRight => true,
            TokenType::Dash => {
                if self.peek_type() == TokenType::GreaterThan {
                    self.next_token();
                    true
                } else {
                    false
                }
            }
            _ => {
                return Err(self.error_at_token(
                    &right_tok,
                    format!("expected -> or -, got {:?}", right_tok.literal),
                ));
            }
        };
        Ok((left_arrow, right_head))
    }

    /// Parses one relationship pattern end to end, as its phase sequence:
    /// the two arrowheads with the optional bracket body between them
    /// ([`Parser::read_relationship_heads`]), the direction they pick,
    /// and the optional GPM postfix quantifier.
    pub(super) fn parse_rel_pattern(&mut self) -> ParseResult<RelPattern> {
        let mut rel = RelPattern {
            variable: None,
            type_expr: None,
            properties: Vec::new(),
            has_properties: false,
            properties_param: None,
            direction: Direction::Both,
            min_hops: None,
            max_hops: None,
            where_expr: None,
            quantified: false,
        };

        let (left_arrow, right_head) = self.read_relationship_heads(&mut rel)?;
        rel.direction = match (left_arrow, right_head) {
            // `<- ... ->`
            (true, true) => Direction::Both,
            // `<- ... -`
            (true, false) => Direction::Left,
            // `- ... ->` (glued or spaced head)
            (false, true) => Direction::Right,
            // `- ... -`
            (false, false) => Direction::Both,
        };

        // A quantifier directly
        // following the relationship pattern (`-[r]->*`, `-->+`,
        // `-[]->{1,6}`, `<-[:R]-{1,3}`, `-[:R]-{2}`) normalizes to an
        // ordinary variable-length relationship pattern with implicit
        // boundary nodes: it declares no element variables of its own to
        // expose as a group, so it reuses the existing var-length matcher
        // rather than becoming a `Quantified` element. `quantified` marks
        // this origin so the engine knows to honor an inline element-pattern
        // WHERE per traversed edge (CIP-28 semantics), unlike the legacy
        // `[r*min..max]` form which this grammar position never touches.
        if self.peek_type().is_quantifier_start() {
            if rel.is_variable_length() {
                return Err(self.error(
                    "NestedQuantification: a relationship pattern cannot be quantified twice",
                ));
            }
            let quantifier = self.parse_quantifier()?;
            rel.min_hops = Some(quantifier.min);
            rel.max_hops = quantifier.max;
            rel.quantified = true;
        }

        Ok(rel)
    }
}

/// The 42N42 sub-path-binding rule, resolved once: a named unquantified
/// group is supported only when the enclosing path is named or the search
/// is selective. Consulted by both `parse_pattern_path` body call sites
/// (the general path and the `shortestPath()`/`allShortestPaths()`
/// function form); the gate itself lives in `parse_parenthesized_subpath`.
fn subpath_binding_allowed(outer_named: bool, selector: Option<&PathSelector>) -> bool {
    outer_named || selector_is_selective(selector)
}

/// Whether a CIP-73 path selector is *selective* in Neo4j's sense: an
/// explicit selector other than `ALL`. `None` (the implicit selector) and
/// `ALL` are the non-selective spellings; `ANY`, `ANY SHORTEST`,
/// `ALL SHORTEST` and `SHORTEST <n>` are selective. Neo4j 2026 accepts a
/// named parenthesized sub-path only under a selective search (42N42).
pub fn selector_is_selective(selector: Option<&PathSelector>) -> bool {
    matches!(
        selector,
        Some(
            PathSelector::Any(_)
                | PathSelector::AnyShortest
                | PathSelector::AllShortest
                | PathSelector::Shortest { .. }
        )
    )
}

mod rel_bracket;
mod subpath;
